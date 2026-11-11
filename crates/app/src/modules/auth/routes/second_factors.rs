//! Second factors: completing a sign-in with a code, reading one's second factors, setting up an
//! authenticator app, new recovery codes and removing the app (the auth spec's Routes, AUTH-054
//! to AUTH-064).

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use platform::{
    authn::Principal,
    errors::Problem,
    http::{Cookies, FieldErrors, Json, Routes, expired_cookie, session_cookie},
    rate_limit::{LimitError, RateLimits},
    types::{Client, Invalid, Secret, Timestamp},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::{SessionResponse, created, delivery};
use crate::modules::auth::{
    error::AuthError,
    model::{AppCode, FactorProof, RecoveryCode, TokenDelivery},
    service::{Auth, Challenge, Completion, SecondFactors, Setup},
};

/// The name of the cookie that holds a browser's sign-in challenge, after `__Host-` (AUTH-058).
pub(super) const CHALLENGE_COOKIE: &str = "sign_in_challenge";

/// The routes of second factors (AUTH-054 to AUTH-064).
pub(super) fn routes() -> Routes<Arc<Auth>> {
    Routes::new()
        .public(
            routes!(complete_sign_in),
            &[
                &AuthError::InvalidChallenge,
                &AuthError::InvalidSecondFactor,
                &LimitError::RateLimited,
            ],
        )
        .user(routes!(second_factors), &[])
        .user(
            routes!(start_setup),
            &[&AuthError::ReauthenticationRequired],
        )
        .user(
            routes!(confirm_setup),
            &[
                &AuthError::WrongCode,
                &AuthError::SetupExpired,
                &LimitError::RateLimited,
            ],
        )
        .user(routes!(remove_app), &[&AuthError::ReauthenticationRequired])
        .user(
            routes!(new_recovery_codes),
            &[
                &AuthError::ReauthenticationRequired,
                &AuthError::NoSecondFactor,
            ],
        )
}

/// The second factor that a request gives, `code` or `recovery_code`, or `None` when it gives
/// neither and `required` is false, or `None` with the refusals in `errors` (AUTH-060, AUTH-065).
pub(super) fn factor_proof(
    errors: &mut FieldErrors,
    code: Option<&str>,
    recovery_code: Option<&str>,
    required: bool,
) -> Option<Option<FactorProof>> {
    match (code, recovery_code) {
        (Some(code), None) => errors
            .check("/code", AppCode::parse(code))
            .map(|code| Some(FactorProof::App(code))),
        (None, Some(code)) => errors
            .check("/recovery_code", RecoveryCode::parse(code))
            .map(|code| Some(FactorProof::Recovery(code))),
        (None, None) if !required => Some(None),
        _ => errors.check(
            "/code",
            Err(Invalid::new(
                "invalid_code",
                "Send the app's code or a recovery code, one of them.",
            )),
        ),
    }
}

/// What completing a sign-in takes (AUTH-059).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompletionRequest {
    /// The challenge that the sign-in answered. A browser leaves it out, and its cookie carries
    /// it.
    #[serde(default)]
    #[schema(min_length = 1, max_length = 64)]
    challenge: Option<String>,
    /// The code that the authenticator app shows, or else `recovery_code`.
    #[serde(default)]
    #[schema(value_type = Option<AppCode>)]
    code: Option<String>,
    /// One of the account's recovery codes, in place of `code`.
    #[serde(default)]
    #[schema(value_type = Option<RecoveryCode>)]
    recovery_code: Option<String>,
}

/// The challenge of a sign-in that waits for its second factor (AUTH-058).
#[derive(Serialize, ToSchema)]
pub(crate) struct ChallengeResponse {
    /// The challenge, which completing the sign-in sends back, unless it went in a cookie.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    challenge: Option<String>,
    /// When the challenge expires: the sign-in completes before then, or starts again.
    expires_at: Timestamp,
}

/// The 200 of a sign-in that waits for its second factor, with the challenge in its body, or in
/// a cookie for a browser (AUTH-058, SEC-07).
pub(super) fn challenged(
    challenge: Challenge,
    delivery: TokenDelivery,
) -> Result<Response, AuthError> {
    let (cookie, token) = match delivery {
        TokenDelivery::Body => (None, Some(challenge.token.expose().clone())),
        TokenDelivery::Cookie => (
            Some(session_cookie(
                CHALLENGE_COOKIE,
                challenge.token.expose(),
                challenge.expires_at,
            )?),
            None,
        ),
    };
    let body = ChallengeResponse {
        challenge: token,
        expires_at: challenge.expires_at,
    };
    let mut response = (StatusCode::OK, axum::Json(body)).into_response();
    if let Some(cookie) = cookie {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

/// Completes a sign-in with the second factor's code.
#[utoipa::path(
    post,
    path = "/v1/sessions/second-factor",
    tag = "auth",
    request_body = CompletionRequest,
    responses(
        (status = 201, description = "The new session.", body = SessionResponse,
            headers(("location" = String, description = "The new session: `/v1/users/me/sessions/current`."))),
    )
)]
async fn complete_sign_in(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    cookies: Cookies,
    headers: HeaderMap,
    Json(request): Json<CompletionRequest>,
) -> Result<Response, Problem<AuthError>> {
    let mut errors = FieldErrors::new();
    let proof = factor_proof(
        &mut errors,
        request.code.as_deref(),
        request.recovery_code.as_deref(),
        true,
    )
    .flatten();
    let proof = errors.finish(proof).map_err(AuthError::from)?;
    let challenge = match request.challenge {
        Some(challenge) => challenge,
        None => match cookies
            .credential(CHALLENGE_COOKIE)
            .map_err(AuthError::from)?
        {
            Some(challenge) => challenge.to_owned(),
            None => return Err(AuthError::InvalidChallenge.into()),
        },
    };
    let completion = Completion {
        challenge: Secret::new(challenge),
        proof,
    };
    let (session, tokens, device) = auth.complete_sign_in(&limits, &client, completion).await?;
    let delivery = delivery(&headers);
    let mut response = created(session, &tokens, &device, delivery)?;
    if delivery == TokenDelivery::Cookie {
        let cookie: HeaderValue = expired_cookie(CHALLENGE_COOKIE).map_err(AuthError::from)?;
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

/// A user's authenticator app, without its secret (AUTH-054).
#[derive(Serialize, ToSchema)]
pub(crate) struct AppResponse {
    /// When its setup was confirmed.
    created_at: Timestamp,
    /// When the last step whose code it accepted began.
    last_used_at: Timestamp,
}

/// A user's second factors (AUTH-054).
#[derive(Serialize, ToSchema)]
pub(crate) struct SecondFactorsResponse {
    /// The authenticator app, or null without one.
    totp: Option<AppResponse>,
    /// How many of the recovery codes still work.
    recovery_codes_left: i64,
}

impl From<SecondFactors> for SecondFactorsResponse {
    fn from(factors: SecondFactors) -> Self {
        Self {
            totp: factors.app.map(|(created_at, last_used_at)| AppResponse {
                created_at,
                last_used_at,
            }),
            recovery_codes_left: factors.recovery_codes_left,
        }
    }
}

/// Reads the caller's second factors.
#[utoipa::path(
    get,
    path = "/v1/users/me/second-factors",
    tag = "auth",
    responses(
        (status = 200, description = "The caller's second factors.", body = SecondFactorsResponse),
    )
)]
async fn second_factors(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
) -> Result<axum::Json<SecondFactorsResponse>, Problem<AuthError>> {
    let factors = auth.second_factors(&client, &principal).await?;
    Ok(axum::Json(factors.into()))
}

/// A setup of an authenticator app, shown once (AUTH-055).
#[derive(Serialize, ToSchema)]
pub(crate) struct SetupResponse {
    /// The app's secret, in base32 without padding, for an app that can't read a QR code.
    secret: String,
    /// The `otpauth://totp/` URI that holds the secret, which a client shows as a QR code.
    uri: String,
    /// When the setup expires, unless a confirmation ends it first.
    expires_at: Timestamp,
}

impl From<Setup> for SetupResponse {
    fn from(setup: Setup) -> Self {
        Self {
            secret: setup.secret.expose().clone(),
            uri: setup.uri.expose().clone(),
            expires_at: setup.expires_at,
        }
    }
}

/// Starts a setup of an authenticator app, once the session has proved who its user is within
/// the last 5 minutes.
#[utoipa::path(
    post,
    path = "/v1/users/me/second-factors/totp/setup",
    tag = "auth",
    responses(
        (status = 200, description = "The setup, with the app's secret.", body = SetupResponse),
    )
)]
async fn start_setup(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
) -> Result<axum::Json<SetupResponse>, Problem<AuthError>> {
    let setup = auth.start_setup(&client, &principal).await?;
    Ok(axum::Json(setup.into()))
}

/// What confirming a setup takes (AUTH-056).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetupConfirmationRequest {
    /// A code that the app shows for the setup's secret.
    #[schema(value_type = AppCode)]
    code: String,
}

/// The recovery codes, shown once (AUTH-056, AUTH-063).
#[derive(Serialize, ToSchema)]
pub(crate) struct RecoveryCodesResponse {
    /// Ten codes, each of which works once in place of the app's code.
    #[schema(min_items = 10, max_items = 10)]
    recovery_codes: Vec<String>,
}

impl RecoveryCodesResponse {
    fn new(codes: &[RecoveryCode]) -> Self {
        Self {
            recovery_codes: codes.iter().map(RecoveryCode::grouped).collect(),
        }
    }
}

/// Confirms the setup with a code of the app: the app becomes the caller's, with new recovery
/// codes.
#[utoipa::path(
    post,
    path = "/v1/users/me/second-factors/totp/setup/confirm",
    tag = "auth",
    request_body = SetupConfirmationRequest,
    responses(
        (status = 200, description = "The new recovery codes.", body = RecoveryCodesResponse),
    )
)]
async fn confirm_setup(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<SetupConfirmationRequest>,
) -> Result<axum::Json<RecoveryCodesResponse>, Problem<AuthError>> {
    let mut errors = FieldErrors::new();
    let code = errors.check("/code", AppCode::parse(&request.code));
    let code = errors.finish(code).map_err(AuthError::from)?;
    let codes = auth
        .confirm_setup(&limits, &client, &principal, code)
        .await?;
    Ok(axum::Json(RecoveryCodesResponse::new(&codes)))
}

/// Removes the caller's authenticator app, with its recovery codes, once the session has proved
/// who its user is within the last 5 minutes.
#[utoipa::path(
    delete,
    path = "/v1/users/me/second-factors/totp",
    tag = "auth",
    responses(
        (status = 204, description = "The account has no authenticator app."),
    )
)]
async fn remove_app(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
) -> Result<Response, Problem<AuthError>> {
    auth.remove_app(&client, &principal).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Replaces the caller's recovery codes with ten new ones, once the session has proved who its
/// user is within the last 5 minutes.
#[utoipa::path(
    post,
    path = "/v1/users/me/second-factors/recovery-codes",
    tag = "auth",
    responses(
        (status = 200, description = "The new recovery codes.", body = RecoveryCodesResponse),
    )
)]
async fn new_recovery_codes(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
) -> Result<axum::Json<RecoveryCodesResponse>, Problem<AuthError>> {
    let codes = auth.new_recovery_codes(&client, &principal).await?;
    Ok(axum::Json(RecoveryCodesResponse::new(&codes)))
}
