//! Re-authenticating: proving who the caller is again, with their password and the second
//! factor of an account that has one, in the session that authenticated the request (the auth
//! spec's Routes, AUTH-021, AUTH-065).

use std::sync::Arc;

use axum::extract::State;
use platform::{
    authn::Principal,
    errors::{Problem, RequestError},
    http::{FieldErrors, Json, Routes},
    rate_limit::{LimitError, RateLimits},
    types::Client,
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::second_factors;
use crate::modules::auth::{
    error::AuthError,
    model::{FactorProof, Password, Session},
    service::Auth,
};

/// The route that re-authenticates (AUTH-021), which a caller whose account must still prove its
/// address may call, to change the address (AUTH-025, AUTH-048).
pub(super) fn routes() -> Routes<Arc<Auth>> {
    Routes::new().verifying(
        routes!(reauthenticate),
        &[
            &AuthError::WrongPassword,
            &AuthError::SecondFactorRequired,
            &AuthError::WrongSecondFactor,
            &LimitError::RateLimited,
        ],
    )
}

/// What re-authenticating takes (AUTH-021, AUTH-065).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReauthenticationRequest {
    /// The caller's password.
    #[schema(min_length = 1, max_length = 128, format = Password)]
    password: String,
    /// The code that the account's authenticator app shows, which an account with an app sends,
    /// or else `recovery_code`.
    #[serde(default)]
    #[schema(value_type = Option<crate::modules::auth::model::AppCode>)]
    code: Option<String>,
    /// One of the account's recovery codes, in place of `code`.
    #[serde(default)]
    #[schema(value_type = Option<crate::modules::auth::model::RecoveryCode>)]
    recovery_code: Option<String>,
}

impl TryFrom<ReauthenticationRequest> for (Password, Option<FactorProof>) {
    type Error = RequestError;

    fn try_from(request: ReauthenticationRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let password = errors.check("/password", Password::presented(request.password));
        let proof = second_factors::factor_proof(
            &mut errors,
            ("/code", request.code.as_deref()),
            request.recovery_code.as_deref(),
            false,
        );
        errors.finish(password.zip(proof))
    }
}

/// Re-authenticates: proves the caller's password again in the session that authenticated the
/// request, for an action that needs it, such as deleting the account.
#[utoipa::path(
    post,
    path = "/v1/users/me/sessions/current/reauthenticate",
    tag = "auth",
    request_body = ReauthenticationRequest,
    responses(
        (status = 200, description = "The session, without its tokens.", body = Session),
    )
)]
async fn reauthenticate(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<ReauthenticationRequest>,
) -> Result<axum::Json<Session>, Problem<AuthError>> {
    let (password, proof) =
        <(Password, Option<FactorProof>)>::try_from(request).map_err(AuthError::from)?;
    let session = auth
        .reauthenticate(&limits, &client, &principal, password, proof)
        .await?;
    Ok(axum::Json(session))
}
