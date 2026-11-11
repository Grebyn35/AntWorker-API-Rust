//! Asking to change the account's email address, and confirming the change with the code sent to
//! the new one (the auth spec's Routes, AUTH-048 to AUTH-053).

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
    model::{ChangeCode, Session},
    service::{Auth, ChangeConfirmation},
};

/// The routes that change the account's address (AUTH-048 to AUTH-053), which a caller whose
/// account must still prove its address may call, to correct one mistyped at registration
/// (AUTH-025).
pub(super) fn routes() -> Routes<Arc<Auth>> {
    Routes::new()
        .verifying(
            routes!(request_change),
            &[
                &AuthError::ReauthenticationRequired,
                &AuthError::EmailInUse,
                &LimitError::RateLimited,
                &RequestError::FeatureDisabled,
            ],
        )
        .verifying(
            routes!(confirm_change),
            &[
                &AuthError::WrongCode,
                &AuthError::CodeExpired,
                &AuthError::EmailInUse,
                &LimitError::RateLimited,
                &RequestError::FeatureDisabled,
            ],
        )
}

/// What asking to change the address takes (AUTH-048).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeRequest {
    /// The address the account is to have, which the code goes to.
    #[schema(value_type = EmailAddress)]
    email: String,
}

impl TryFrom<ChangeRequest> for EmailAddress {
    type Error = RequestError;

    fn try_from(request: ChangeRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let email = errors.check("/email", EmailAddress::parse(&request.email));
        errors.finish(email)
    }
}

/// What confirming a change of the address takes (AUTH-051).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeConfirmationRequest {
    /// The address the change asked for, which received the code.
    #[schema(value_type = EmailAddress)]
    email: String,
    /// The code that the address received.
    #[schema(value_type = ChangeCode)]
    code: String,
}

impl TryFrom<ChangeConfirmationRequest> for ChangeConfirmation {
    type Error = RequestError;

    fn try_from(request: ChangeConfirmationRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let email = errors.check("/email", EmailAddress::parse(&request.email));
        let code = errors.check("/code", ChangeCode::parse(&request.code));
        let built = email.zip(code).map(|(email, code)| Self { email, code });
        errors.finish(built)
    }
}

/// Where a code that changes the account's address goes (AUTH-048).
#[derive(Serialize, ToSchema)]
pub(crate) struct EmailChange {
    /// The address asked for, normalized, which the email with the code goes to.
    #[schema(value_type = EmailAddress)]
    email: String,
}

/// Asks to change the account's email address to another, which an email to it with a code
/// proves, in place of any change asked for before. The session must have proved its password
/// within the last 5 minutes.
#[utoipa::path(
    post,
    path = "/v1/users/me/email/change",
    tag = "auth",
    request_body = ChangeRequest,
    responses(
        (status = 202, description = "The email with the code is on its way to the address asked for.", body = EmailChange),
    )
)]
async fn request_change(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<ChangeRequest>,
) -> Result<Response, Problem<AuthError>> {
    let email = EmailAddress::try_from(request).map_err(AuthError::from)?;
    let email = auth
        .request_email_change(&limits, &client, &principal, email)
        .await?;
    let body = EmailChange {
        email: email.as_str().to_owned(),
    };
    Ok((StatusCode::ACCEPTED, axum::Json(body)).into_response())
}

/// Changes the account's email address to the one asked for, with the code sent to it, which
/// proves it.
#[utoipa::path(
    post,
    path = "/v1/users/me/email/change/confirm",
    tag = "auth",
    request_body = ChangeConfirmationRequest,
    responses(
        (status = 200, description = "The session, without its tokens, whose account's address is the new one, which it has proved.", body = Session),
    )
)]
async fn confirm_change(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<ChangeConfirmationRequest>,
) -> Result<axum::Json<Session>, Problem<AuthError>> {
    let confirmation = ChangeConfirmation::try_from(request).map_err(AuthError::from)?;
    let session = auth
        .confirm_email_change(&limits, &client, &principal, confirmation)
        .await?;
    Ok(axum::Json(session))
}
