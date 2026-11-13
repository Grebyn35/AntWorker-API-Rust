//! Signing in, registering and re-authenticating through an OAuth provider, and linking one to
//! the caller's account (the auth spec's Routes, AUTH-073 to AUTH-080, AUTH-087).

use std::sync::Arc;

use axum::{extract::State, http::HeaderMap, response::Response};
use platform::{
    authn::Principal,
    errors::{Problem, RequestError},
    http::{Cookies, FieldErrors, Json, Routes},
    rate_limit::{LimitError, RateLimits},
    types::{Client, Invalid, Secret},
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::{created, delivery, device, second_factors};
use crate::modules::auth::{
    error::AuthError,
    model::{DEVICE_COOKIE, DeviceId, DeviceName, FactorProof, Identities, Session},
    providers::Providers,
    service::{Auth, Grant, ProviderSignIn, SignedIn},
};

/// The longest code that a request carries (AUTH-077).
const CODE_MAX_CHARS: usize = 2_048;

/// The shortest and longest verifier of a PKCE challenge, as RFC 7636 allows them (AUTH-077).
const VERIFIER_CHARS: std::ops::RangeInclusive<usize> = 43..=128;

/// The state of the routes that sign in, re-authenticate and link through a provider: the
/// service, and the providers that the deployment configures, whose clients' redirect URIs a
/// request's conversion reads (AUTH-078, API-10).
#[derive(Clone)]
pub(crate) struct ProviderRoutes {
    auth: Arc<Auth>,
    providers: Arc<Providers>,
}

impl ProviderRoutes {
    /// The state of `auth`, and of `providers`, which it was built with.
    pub(crate) fn new(auth: Arc<Auth>, providers: Arc<Providers>) -> Self {
        Self { auth, providers }
    }
}

/// The routes that sign in, re-authenticate and link through a provider; re-authenticating
/// serves a caller whose account must still prove its address, to change the address (AUTH-025,
/// AUTH-080).
pub(crate) fn provider_routes() -> Routes<ProviderRoutes> {
    let exchange: [&dyn platform::errors::ErrorCode; 3] = [
        &AuthError::ProviderNotFound,
        &AuthError::ProviderUnavailable(platform::errors::Internal::new("the provider")),
        &RequestError::FeatureDisabled,
    ];
    let mut sign_in = exchange.to_vec();
    sign_in.extend([
        &AuthError::ProviderCodeRejected as &dyn platform::errors::ErrorCode,
        &AuthError::EmailInUse,
        &AuthError::IdentityInUse,
        &AuthError::ProviderAddressMissing,
        &LimitError::RateLimited,
    ]);
    let mut reauthentication = exchange.to_vec();
    reauthentication.extend([
        &AuthError::ProviderProofRejected as &dyn platform::errors::ErrorCode,
        &AuthError::WrongIdentity,
        &AuthError::SecondFactorRequired,
        &AuthError::WrongSecondFactor,
        &LimitError::RateLimited,
    ]);
    let mut link = exchange.to_vec();
    link.extend([
        &AuthError::ProviderProofRejected as &dyn platform::errors::ErrorCode,
        &AuthError::ReauthenticationRequired,
        &AuthError::ProviderAlreadyLinked,
        &AuthError::IdentityInUse,
        &LimitError::RateLimited,
    ]);
    Routes::new()
        .public(routes!(sign_in_through), &sign_in)
        .verifying(routes!(reauthenticate_through), &reauthentication)
        .user(routes!(link_provider), &link)
}

/// What signing in through a provider takes (AUTH-073).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderSignInRequest {
    /// The provider's key, as `sign_in_providers` lists it.
    #[schema(min_length = 1, max_length = 64, pattern = "^[a-z][a-z0-9_]*$")]
    provider: String,
    /// The code that the provider's authorization gave the redirect URI.
    #[schema(min_length = 1, max_length = 2048)]
    code: String,
    /// The verifier of the authorization request's PKCE challenge (RFC 7636).
    #[schema(min_length = 43, max_length = 128, pattern = "^[A-Za-z0-9._~-]+$")]
    code_verifier: String,
    /// The redirect URI that received the code, one that the provider's clients list (AUTH-078).
    #[schema(min_length = 1, max_length = 2048)]
    redirect_uri: String,
    /// An ID the client keeps for its installation (AUTH-008, AUTH-017).
    #[serde(default)]
    #[schema(value_type = Option<DeviceId>)]
    device_id: Option<String>,
    /// A name for the device, such as `Ada's laptop`, which the list of the user's sessions shows
    /// (AUTH-042).
    #[serde(default)]
    #[schema(value_type = Option<DeviceName>)]
    device_name: Option<String>,
    /// The device token that the account's last sign-in on this device returned (AUTH-019). A
    /// browser leaves it out, and its cookie carries it. One that isn't valid for the account's
    /// email counts as none.
    #[serde(default)]
    #[schema(min_length = 1, max_length = 64)]
    device_token: Option<String>,
}

/// What re-authenticating through a provider takes (AUTH-080).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderReauthenticationRequest {
    /// The provider's key, as `sign_in_providers` lists it.
    #[schema(min_length = 1, max_length = 64, pattern = "^[a-z][a-z0-9_]*$")]
    provider: String,
    /// The code that the provider's authorization gave the redirect URI.
    #[schema(min_length = 1, max_length = 2048)]
    code: String,
    /// The verifier of the authorization request's PKCE challenge (RFC 7636).
    #[schema(min_length = 43, max_length = 128, pattern = "^[A-Za-z0-9._~-]+$")]
    code_verifier: String,
    /// The redirect URI that received the code, one that the provider's clients list (AUTH-078).
    #[schema(min_length = 1, max_length = 2048)]
    redirect_uri: String,
    /// The code that the account's authenticator app shows, which an account with an app sends,
    /// or else `recovery_code`.
    #[serde(default)]
    #[schema(value_type = Option<crate::modules::auth::model::AppCode>)]
    second_factor_code: Option<String>,
    /// One of the account's recovery codes, in place of `second_factor_code`.
    #[serde(default)]
    #[schema(value_type = Option<crate::modules::auth::model::RecoveryCode>)]
    recovery_code: Option<String>,
}

/// What linking a provider takes (AUTH-087).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderLinkRequest {
    /// The provider's key, as `sign_in_providers` lists it.
    #[schema(min_length = 1, max_length = 64, pattern = "^[a-z][a-z0-9_]*$")]
    provider: String,
    /// The code that the provider's authorization gave the redirect URI.
    #[schema(min_length = 1, max_length = 2048)]
    code: String,
    /// The verifier of the authorization request's PKCE challenge (RFC 7636).
    #[schema(min_length = 43, max_length = 128, pattern = "^[A-Za-z0-9._~-]+$")]
    code_verifier: String,
    /// The redirect URI that received the code, one that the provider's clients list (AUTH-078).
    #[schema(min_length = 1, max_length = 2048)]
    redirect_uri: String,
}

/// The grant of `code`, `verifier` and `redirect_uri` at `provider`, or `None` when the code or
/// the verifier is malformed, or the provider's clients don't list the redirect URI, which
/// `errors` then holds, so one 400 names every field (AUTH-077, AUTH-078, API-10).
fn grant(
    errors: &mut FieldErrors,
    providers: &Providers,
    provider: &str,
    (code, verifier, redirect_uri): (String, String, String),
) -> Option<Grant> {
    let code_valid = !code.is_empty()
        && code.chars().count() <= CODE_MAX_CHARS
        && code.chars().all(|c| c.is_ascii_graphic());
    let code = errors.check(
        "/code",
        code_valid.then_some(code).ok_or_else(|| {
            Invalid::new(
                "invalid_code",
                "The code must be 1 to 2,048 visible ASCII characters.",
            )
        }),
    );
    let verifier_valid = VERIFIER_CHARS.contains(&verifier.len())
        && verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'));
    let verifier = errors.check(
        "/code_verifier",
        verifier_valid.then_some(verifier).ok_or_else(|| {
            Invalid::new(
                "invalid_code_verifier",
                "The verifier must be 43 to 128 letters, digits, hyphens, periods, underscores \
                 and tildes (RFC 7636).",
            )
        }),
    );
    let listed = errors.check(
        "/redirect_uri",
        providers
            .lists(provider, &redirect_uri)
            .then_some(())
            .ok_or_else(|| {
                Invalid::new(
                    "invalid_redirect_uri",
                    "The redirect URI is none that the provider's clients list.",
                )
            }),
    );
    code.zip(verifier)
        .zip(listed)
        .map(|((code, verifier), ())| Grant {
            code: Secret::new(code),
            verifier: Secret::new(verifier),
            redirect_uri,
        })
}

/// Signs in through a provider, or registers through it when no account is linked to the user's
/// account there and none holds its address.
#[utoipa::path(
    post,
    path = "/v1/sessions/provider",
    tag = "auth",
    request_body = ProviderSignInRequest,
    responses(
        (status = 201, description = "The new session, of the account that the provider's account signs in to, or of the account it registered.", body = super::SessionResponse,
            headers(("location" = String, description = "The new session: `/v1/users/me/sessions/current`."))),
        (status = 200, description = "The challenge that the account's second factor completes, at `/v1/sessions/second-factor`.", body = second_factors::ChallengeResponse),
    )
)]
async fn sign_in_through(
    State(ProviderRoutes { auth, providers }): State<ProviderRoutes>,
    limits: RateLimits,
    client: Client,
    cookies: Cookies,
    headers: HeaderMap,
    Json(request): Json<ProviderSignInRequest>,
) -> Result<Response, Problem<AuthError>> {
    let mut errors = FieldErrors::new();
    let grant = grant(
        &mut errors,
        &providers,
        &request.provider,
        (request.code, request.code_verifier, request.redirect_uri),
    );
    let device = device(
        &mut errors,
        request.device_id.as_deref(),
        request.device_name.as_deref(),
    );
    let (grant, mut device) = errors.finish(grant.zip(device)).map_err(AuthError::from)?;
    let provider = request.provider;
    device.provider = Some(provider.clone());
    let input = ProviderSignIn {
        provider,
        grant,
        device,
        device_token: request.device_token.map(Secret::new),
        device_cookies: cookies
            .starting_with(DEVICE_COOKIE)
            .into_iter()
            .map(|(name, token)| (name.to_owned(), Secret::new(token.to_owned())))
            .collect(),
    };
    match auth.sign_in_through(&limits, &client, input).await? {
        SignedIn::Session(started) => {
            let (session, tokens, device) = *started;
            Ok(created(session, &tokens, &device, delivery(&headers))?)
        }
        SignedIn::Challenged(challenge) => {
            Ok(second_factors::challenged(challenge, delivery(&headers))?)
        }
    }
}

/// Re-authenticates through a provider: proves again, in the session that authenticated the
/// request, that the caller holds their account at the provider, for an action that needs it,
/// such as deleting the account.
#[utoipa::path(
    post,
    path = "/v1/users/me/sessions/current/reauthenticate/provider",
    tag = "auth",
    request_body = ProviderReauthenticationRequest,
    responses(
        (status = 200, description = "The session, without its tokens.", body = Session),
    )
)]
async fn reauthenticate_through(
    State(ProviderRoutes { auth, providers }): State<ProviderRoutes>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<ProviderReauthenticationRequest>,
) -> Result<axum::Json<Session>, Problem<AuthError>> {
    let mut errors = FieldErrors::new();
    let grant = grant(
        &mut errors,
        &providers,
        &request.provider,
        (request.code, request.code_verifier, request.redirect_uri),
    );
    let proof = second_factors::factor_proof(
        &mut errors,
        ("/second_factor_code", request.second_factor_code.as_deref()),
        request.recovery_code.as_deref(),
        false,
    );
    let (grant, proof): (Grant, Option<FactorProof>) =
        errors.finish(grant.zip(proof)).map_err(AuthError::from)?;
    let provider = request.provider;
    let session = auth
        .reauthenticate_through(&limits, &client, &principal, (&provider, grant), proof)
        .await?;
    Ok(axum::Json(session))
}

/// Links the caller's account at a provider, which the code names, to their account, so it signs
/// in to it, once the session has proved who its user is within the last 5 minutes.
#[utoipa::path(
    post,
    path = "/v1/users/me/identities/provider",
    tag = "auth",
    request_body = ProviderLinkRequest,
    responses(
        (status = 200, description = "The caller's identities, the provider's account among them.", body = Identities),
    )
)]
async fn link_provider(
    State(ProviderRoutes { auth, providers }): State<ProviderRoutes>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<ProviderLinkRequest>,
) -> Result<axum::Json<Identities>, Problem<AuthError>> {
    let mut errors = FieldErrors::new();
    let grant = grant(
        &mut errors,
        &providers,
        &request.provider,
        (request.code, request.code_verifier, request.redirect_uri),
    );
    let grant = errors.finish(grant).map_err(AuthError::from)?;
    let identities = auth
        .link_provider(&limits, &client, &principal, (&request.provider, grant))
        .await?;
    Ok(axum::Json(identities))
}
