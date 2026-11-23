//! Registration, sign-in with a password, refresh and deleting the account (the auth spec's
//! Routes). Signing in, re-authenticating and linking an identity through a provider is in
//! `providers.rs`, and how a session's tokens reach its client in `delivery.rs`. Reading,
//! listing and ending a user's sessions, signing out among them, is in `sessions.rs`, proving
//! the account's email address in `verification.rs`, resetting and changing the password in
//! `password.rs`, changing the address in `email_change.rs`, re-authenticating with the password
//! in `reauthentication.rs`, second factors, completing a sign-in with one among them, in
//! `second_factors.rs`, the caller's identities in `identities.rs`, and requesting an export of
//! the caller's data, which this module confirms, in `exports.rs`.

mod delivery;
mod email_change;
mod exports;
mod identities;
mod password;
mod providers;
mod reauthentication;
mod second_factors;
mod sessions;
mod verification;

use std::sync::Arc;

use axum::{extract::State, http::HeaderMap, response::Response};
use platform::{
    authn::Principal,
    errors::{Problem, RequestError},
    http::{Cookies, FieldErrors, Json, Routes},
    rate_limit::RateLimits,
    types::{Client, EmailAddress, Invalid, Secret},
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::{
    error::AuthError,
    events::{SESSION_CREATED, SESSION_ENDED},
    model::{DEVICE_COOKIE, DeviceId, DeviceName, Password},
    service::{Auth, Device, Held, Registration, SignIn, SignedIn},
};
use crate::modules::accounts::DisplayName;

pub(crate) use delivery::SessionResponse;
use delivery::{REFRESH_COOKIE, created, delivery, held, renewed, signed_out};
pub(crate) use exports::{ExportRequests, export_routes};
pub(crate) use providers::{ProviderRoutes, provider_routes};

/// The module's routes, which take the service as their state.
pub(crate) fn routes() -> Routes<Arc<Auth>> {
    verification::routes()
        .merge(email_change::routes())
        .merge(password::routes())
        .merge(sessions::routes())
        .merge(second_factors::routes())
        .merge(reauthentication::routes())
        .merge(identities::routes())
        .events(&[&SESSION_CREATED, &SESSION_ENDED])
        .public(routes!(register), &[&AuthError::EmailInUse])
        .public(routes!(sign_in), &[&AuthError::InvalidCredentials])
        .public(
            routes!(refresh),
            &[&AuthError::SessionExpired, &AuthError::InvalidRefreshToken],
        )
        .user(
            routes!(delete_account),
            &[
                &RequestError::ReauthenticationRequired,
                &AuthError::OwnsSharedWorkspace,
                &AuthError::AccountChanged,
            ],
        )
}

/// What registering takes (AUTH-001).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistrationRequest {
    /// The user's email address.
    #[schema(value_type = EmailAddress)]
    email: String,
    /// The user's password (AUTH-003).
    #[schema(value_type = Password)]
    password: String,
    /// The user's name.
    #[schema(value_type = DisplayName)]
    name: String,
    /// An ID the client keeps for its installation (AUTH-008, AUTH-017).
    #[serde(default)]
    #[schema(value_type = Option<DeviceId>)]
    device_id: Option<String>,
    /// A name for the device, such as `Ada's laptop`, which the list of the user's sessions shows
    /// (AUTH-042).
    #[serde(default)]
    #[schema(value_type = Option<DeviceName>)]
    device_name: Option<String>,
}

impl TryFrom<RegistrationRequest> for Registration {
    type Error = RequestError;

    fn try_from(request: RegistrationRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let email = errors.check("/email", EmailAddress::parse(&request.email));
        let password = errors.check("/password", Password::parse(request.password));
        let name = errors.check("/name", DisplayName::parse(&request.name));
        let device = device(
            &mut errors,
            request.device_id.as_deref(),
            request.device_name.as_deref(),
        );
        let built =
            email
                .zip(password)
                .zip(name)
                .zip(device)
                .map(|(((email, password), name), device)| Self {
                    email,
                    password,
                    name,
                    device,
                });
        errors.finish(built)
    }
}

/// What signing in takes (AUTH-004).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SignInRequest {
    /// The account's email address.
    #[schema(value_type = EmailAddress)]
    email: String,
    /// The account's password.
    #[schema(min_length = 1, max_length = 128, format = Password)]
    password: String,
    /// An ID the client keeps for its installation (AUTH-008, AUTH-017).
    #[serde(default)]
    #[schema(value_type = Option<DeviceId>)]
    device_id: Option<String>,
    /// A name for the device, such as `Ada's laptop`, which the list of the user's sessions shows
    /// (AUTH-042).
    #[serde(default)]
    #[schema(value_type = Option<DeviceName>)]
    device_name: Option<String>,
    /// The device token that the account's last registration or sign-in on this device returned
    /// (AUTH-019). A browser leaves it out, and its cookie carries it. One of this length that
    /// isn't valid for the email counts as none.
    #[serde(default)]
    #[schema(min_length = 1, max_length = 64)]
    device_token: Option<String>,
}

impl TryFrom<SignInRequest> for SignIn {
    type Error = RequestError;

    fn try_from(request: SignInRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let email = errors.check("/email", EmailAddress::parse(&request.email));
        let password = errors.check("/password", Password::presented(request.password));
        let device = device(
            &mut errors,
            request.device_id.as_deref(),
            request.device_name.as_deref(),
        );
        let device_token = body_token(
            &mut errors,
            ("/device_token", "invalid_device_token"),
            request.device_token,
        );
        let built = email.zip(password).zip(device).zip(device_token).map(
            |(((email, password), device), device_token)| Self {
                email,
                password,
                device,
                device_token,
                device_cookies: Vec::new(),
            },
        );
        errors.finish(built)
    }
}

/// The longest token that a body gives where a browser's cookie carries it, as the body's schema
/// states: a device token, a refresh token or a challenge, each of which the API issues with 43
/// characters (API-17).
const TOKEN_MAX_CHARS: usize = 64;

/// `token`, a token that a body gives at the JSON Pointer `pointer`, or none when the body gives
/// none; or `None`, which `errors` then holds with `code`, when it is empty or longer than
/// [`TOKEN_MAX_CHARS`] (AUTH-010, AUTH-019, AUTH-060).
fn body_token(
    errors: &mut FieldErrors,
    (pointer, code): (&str, &'static str),
    token: Option<String>,
) -> Option<Option<Secret<String>>> {
    let Some(token) = token else {
        return Some(None);
    };
    let bounded = (1..=TOKEN_MAX_CHARS).contains(&token.chars().count());
    let checked = bounded
        .then(|| Secret::new(token))
        .ok_or_else(|| Invalid::new(code, "The token must be 1 to 64 characters."));
    errors.check(pointer, checked).map(Some)
}

/// The device of `id` and `name`, either of which may be absent, or `None` when either is
/// invalid, which `errors` then holds.
fn device(errors: &mut FieldErrors, id: Option<&str>, name: Option<&str>) -> Option<Device> {
    let id = match id {
        None => Some(None),
        Some(value) => errors.check("/device_id", DeviceId::parse(value)).map(Some),
    };
    let name = match name {
        None => Some(None),
        Some(value) => errors
            .check("/device_name", DeviceName::parse(value))
            .map(Some),
    };
    id.zip(name).map(|(id, name)| Device {
        id,
        name,
        provider: None,
        held: Held::default(),
    })
}

/// What refreshing takes: the refresh token, unless a browser's cookie holds it (AUTH-010).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefreshRequest {
    /// The session's current refresh token. A browser leaves it out, and its cookie carries it.
    #[serde(default)]
    #[schema(min_length = 1, max_length = 64)]
    refresh_token: Option<String>,
}

/// The refresh token that the body gives, if any (AUTH-010).
impl TryFrom<RefreshRequest> for Option<Secret<String>> {
    type Error = RequestError;

    fn try_from(request: RefreshRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let token = body_token(
            &mut errors,
            ("/refresh_token", "invalid_refresh_token"),
            request.refresh_token,
        );
        errors.finish(token)
    }
}

/// Registers a user, and signs them in.
#[utoipa::path(
    post,
    path = "/v1/users",
    tag = "auth",
    request_body = RegistrationRequest,
    responses(
        (status = 201, description = "The user's first session.", body = SessionResponse,
            headers(("location" = String, description = "The new session: `/v1/users/me/sessions/current`."))),
    )
)]
async fn register(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    cookies: Cookies,
    headers: HeaderMap,
    Json(request): Json<RegistrationRequest>,
) -> Result<Response, Problem<AuthError>> {
    let mut input = Registration::try_from(request).map_err(AuthError::from)?;
    input.device.held = held(&cookies);
    let (session, tokens, device) = auth.register(&limits, &client, input).await?;
    Ok(created(session, &tokens, &device, delivery(&headers))?)
}

/// Signs in with an email and a password.
#[utoipa::path(
    post,
    path = "/v1/sessions",
    tag = "auth",
    request_body = SignInRequest,
    responses(
        (status = 201, description = "The new session.", body = SessionResponse,
            headers(("location" = String, description = "The new session: `/v1/users/me/sessions/current`."))),
        (status = 200, description = "The challenge that the account's second factor completes, at `/v1/sessions/second-factor`.", body = second_factors::ChallengeResponse),
    )
)]
async fn sign_in(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    cookies: Cookies,
    headers: HeaderMap,
    Json(request): Json<SignInRequest>,
) -> Result<Response, Problem<AuthError>> {
    let mut input = SignIn::try_from(request).map_err(AuthError::from)?;
    input.device.held = held(&cookies);
    input.device_cookies = cookies
        .starting_with(DEVICE_COOKIE)
        .into_iter()
        .map(|(name, token)| (name.to_owned(), Secret::new(token.to_owned())))
        .collect();
    match auth.sign_in(&limits, &client, input).await? {
        SignedIn::Session(started) => {
            let (session, tokens, device) = *started;
            Ok(created(session, &tokens, &device, delivery(&headers))?)
        }
        SignedIn::Challenged(challenge) => {
            Ok(second_factors::challenged(challenge, delivery(&headers))?)
        }
    }
}

/// Gives the session a new pair of tokens.
#[utoipa::path(
    post,
    path = "/v1/sessions/refresh",
    tag = "auth",
    request_body = RefreshRequest,
    responses(
        (status = 200, description = "The session, with its new tokens.", body = SessionResponse),
    )
)]
async fn refresh(
    State(auth): State<Arc<Auth>>,
    client: Client,
    cookies: Cookies,
    headers: HeaderMap,
    Json(request): Json<RefreshRequest>,
) -> Result<Response, Problem<AuthError>> {
    let token = match <Option<Secret<String>>>::try_from(request).map_err(AuthError::from)? {
        Some(token) => token,
        None => match cookies
            .credential(REFRESH_COOKIE)
            .map_err(AuthError::from)?
        {
            Some(token) => Secret::new(token.to_owned()),
            None => return Err(AuthError::InvalidRefreshToken.into()),
        },
    };
    let (session, tokens) = auth.refresh(&client, token.expose()).await?;
    Ok(renewed(session, &tokens, delivery(&headers))?)
}

/// Deletes the caller's account, and everything it holds, once the session that authenticated
/// the request has proved who they are within the last 5 minutes.
#[utoipa::path(
    delete,
    path = "/v1/users/me",
    tag = "auth",
    responses(
        (status = 204, description = "The account is deleted, and every session of it ended."),
    )
)]
async fn delete_account(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
) -> Result<Response, Problem<AuthError>> {
    auth.delete_account(&limits, &client, &principal).await?;
    Ok(signed_out()?)
}
