//! Resetting a forgotten password, and changing the signed-in user's (the auth spec's Routes,
//! AUTH-034 to AUTH-041).

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use platform::{
    authn::Principal,
    errors::{Problem, RequestError},
    http::{FieldErrors, Json, Routes},
    rate_limit::{LimitError, RateLimits},
    types::{Client, EmailAddress},
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use crate::modules::auth::{
    error::AuthError,
    model::{Password, ResetCode},
    service::{Auth, PasswordChange, ResetConfirmation},
};

/// The routes that reset and change a password (AUTH-034 to AUTH-041).
pub(super) fn routes() -> Routes<Arc<Auth>> {
    Routes::new()
        .public(
            routes!(request_reset),
            &[&LimitError::RateLimited, &RequestError::FeatureDisabled],
        )
        .public(
            routes!(confirm_reset),
            &[
                &AuthError::InvalidResetCode,
                &AuthError::SecondFactorMissing,
                &AuthError::InvalidSecondFactor,
                &LimitError::RateLimited,
                &RequestError::FeatureDisabled,
            ],
        )
        .user(
            routes!(change_password),
            &[
                &AuthError::WrongPassword,
                &AuthError::SecondFactorRequired,
                &AuthError::WrongSecondFactor,
                &LimitError::RateLimited,
            ],
        )
}

/// What asking for a reset takes (AUTH-034).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResetRequest {
    /// The account's email address, which the reset code goes to.
    #[schema(value_type = EmailAddress)]
    email: String,
}

impl TryFrom<ResetRequest> for EmailAddress {
    type Error = RequestError;

    fn try_from(request: ResetRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let email = errors.check("/email", EmailAddress::parse(&request.email));
        errors.finish(email)
    }
}

/// What confirming a reset takes (AUTH-037).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResetConfirmationRequest {
    /// The account's email address.
    #[schema(value_type = EmailAddress)]
    email: String,
    /// The reset code that the address received.
    #[schema(value_type = ResetCode)]
    code: String,
    /// The account's new password (AUTH-003).
    #[schema(value_type = Password)]
    password: String,
    /// The code that the account's authenticator app shows, which an account with an app sends,
    /// or else `recovery_code` (AUTH-037).
    #[serde(default)]
    #[schema(value_type = Option<crate::modules::auth::model::AppCode>)]
    second_factor_code: Option<String>,
    /// One of the account's recovery codes, in place of `second_factor_code`.
    #[serde(default)]
    #[schema(value_type = Option<crate::modules::auth::model::RecoveryCode>)]
    recovery_code: Option<String>,
}

impl TryFrom<ResetConfirmationRequest> for ResetConfirmation {
    type Error = RequestError;

    fn try_from(request: ResetConfirmationRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let email = errors.check("/email", EmailAddress::parse(&request.email));
        let code = errors.check("/code", ResetCode::parse(&request.code));
        let password = errors.check("/password", Password::parse(request.password));
        let proof = super::second_factors::factor_proof(
            &mut errors,
            ("/second_factor_code", request.second_factor_code.as_deref()),
            request.recovery_code.as_deref(),
            false,
        );
        let built =
            email
                .zip(code)
                .zip(password)
                .zip(proof)
                .map(|(((email, code), password), proof)| Self {
                    email,
                    code,
                    password,
                    proof,
                });
        errors.finish(built)
    }
}

/// What changing the password takes (AUTH-040).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PasswordChangeRequest {
    /// The caller's current password.
    #[schema(min_length = 1, max_length = 128, format = Password)]
    current_password: String,
    /// The caller's new password (AUTH-003).
    #[schema(value_type = Password)]
    password: String,
    /// The code that the account's authenticator app shows, which an account with an app sends,
    /// or else `recovery_code` (AUTH-065).
    #[serde(default)]
    #[schema(value_type = Option<crate::modules::auth::model::AppCode>)]
    code: Option<String>,
    /// One of the account's recovery codes, in place of `code`.
    #[serde(default)]
    #[schema(value_type = Option<crate::modules::auth::model::RecoveryCode>)]
    recovery_code: Option<String>,
}

impl TryFrom<PasswordChangeRequest> for PasswordChange {
    type Error = RequestError;

    fn try_from(request: PasswordChangeRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let current = errors.check(
            "/current_password",
            Password::presented(request.current_password),
        );
        let password = errors.check("/password", Password::parse(request.password));
        let proof = super::second_factors::factor_proof(
            &mut errors,
            ("/code", request.code.as_deref()),
            request.recovery_code.as_deref(),
            false,
        );
        let built = current
            .zip(password)
            .zip(proof)
            .map(|((current, password), proof)| Self {
                current,
                password,
                proof,
            });
        errors.finish(built)
    }
}

/// Asks for a code that resets the password of the account that holds the address, which an
/// email to the address carries, and which works for 10 minutes beside any other reset code sent
/// to it. The answer is the same whether or not an account holds the address.
#[utoipa::path(
    post,
    path = "/v1/password-resets",
    tag = "auth",
    request_body = ResetRequest,
    responses(
        (status = 202, description = "If an account holds the address, the email with its reset code is on its way to it."),
    )
)]
async fn request_reset(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    Json(request): Json<ResetRequest>,
) -> Result<Response, Problem<AuthError>> {
    let email = EmailAddress::try_from(request).map_err(AuthError::from)?;
    auth.request_reset(&limits, &client, email).await?;
    Ok(StatusCode::ACCEPTED.into_response())
}

/// Resets the password of the account that holds the address with the reset code sent to it,
/// and, for an account with an authenticator app, the app's code or a recovery code, ends every
/// session of the account, and proves its address. It starts no session: sign in with the new
/// password.
#[utoipa::path(
    post,
    path = "/v1/password-resets/confirm",
    tag = "auth",
    request_body = ResetConfirmationRequest,
    responses(
        (status = 204, description = "The password is reset, and every session of the account ended."),
    )
)]
async fn confirm_reset(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    Json(request): Json<ResetConfirmationRequest>,
) -> Result<Response, Problem<AuthError>> {
    let confirmation = ResetConfirmation::try_from(request).map_err(AuthError::from)?;
    auth.confirm_reset(&limits, &client, confirmation).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Changes the caller's password, given their current one, and ends every other session of
/// theirs.
#[utoipa::path(
    post,
    path = "/v1/users/me/password/change",
    tag = "auth",
    request_body = PasswordChangeRequest,
    responses(
        (status = 204, description = "The password changed, and every other session of the caller ended."),
    )
)]
async fn change_password(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<PasswordChangeRequest>,
) -> Result<Response, Problem<AuthError>> {
    let change = PasswordChange::try_from(request).map_err(AuthError::from)?;
    auth.change_password(&limits, &client, &principal, change)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
