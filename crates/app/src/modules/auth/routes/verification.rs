//! Asking for a code that proves the account's email address, and confirming it (the auth spec's
//! Routes, AUTH-028 to AUTH-033).

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
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::routes;

use crate::modules::auth::{
    error::AuthError,
    model::{Session, VerificationCode},
    service::Auth,
};

/// The routes that prove the account's email address (AUTH-028 to AUTH-033), which a caller whose
/// account must still prove it may call (AUTH-025).
pub(super) fn routes() -> Routes<Arc<Auth>> {
    Routes::new()
        .verifying(
            routes!(request_code),
            &[
                &AuthError::EmailAlreadyVerified,
                &LimitError::RateLimited,
                &RequestError::FeatureDisabled,
            ],
        )
        .verifying(
            routes!(confirm_code),
            &[
                &AuthError::EmailAlreadyVerified,
                &AuthError::WrongCode,
                &AuthError::CodeExpired,
                &LimitError::RateLimited,
                &RequestError::FeatureDisabled,
            ],
        )
}

/// What confirming a code takes (AUTH-031).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfirmationRequest {
    /// The code that the account's address received.
    #[schema(value_type = VerificationCode)]
    code: String,
}

impl TryFrom<ConfirmationRequest> for VerificationCode {
    type Error = RequestError;

    fn try_from(request: ConfirmationRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let code = errors.check("/code", VerificationCode::parse(&request.code));
        errors.finish(code)
    }
}

/// Where a code that proves the account's address goes (AUTH-028).
#[derive(Serialize, ToSchema)]
pub(crate) struct EmailVerification {
    /// The account's address, which the email with the code goes to.
    #[schema(value_type = EmailAddress)]
    email: String,
}

/// Asks for a code that proves the account's email address, which an email to the address
/// carries, in place of any code before it.
#[utoipa::path(
    post,
    path = "/v1/users/me/email/verification",
    tag = "auth",
    responses(
        (status = 202, description = "The email with the code is on its way to the address.", body = EmailVerification),
    )
)]
async fn request_code(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
) -> Result<Response, Problem<AuthError>> {
    let email = auth.request_code(&limits, &client, &principal).await?;
    let body = EmailVerification {
        email: email.as_str().to_owned(),
    };
    Ok((StatusCode::ACCEPTED, axum::Json(body)).into_response())
}

/// Proves the account's email address with the code sent to it.
#[utoipa::path(
    post,
    path = "/v1/users/me/email/verification/confirm",
    tag = "auth",
    request_body = ConfirmationRequest,
    responses(
        (status = 200, description = "The session, without its tokens, whose account no longer needs to verify its address.", body = Session),
    )
)]
async fn confirm_code(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<ConfirmationRequest>,
) -> Result<axum::Json<Session>, Problem<AuthError>> {
    let code = VerificationCode::try_from(request).map_err(AuthError::from)?;
    let session = auth
        .confirm_code(&limits, &client, &principal, code)
        .await?;
    Ok(axum::Json(session))
}
