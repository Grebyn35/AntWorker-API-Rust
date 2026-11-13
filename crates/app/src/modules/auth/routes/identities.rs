//! A user's identities: reading them, adding a password and removing one (the auth spec's
//! Routes, AUTH-085, AUTH-086, AUTH-088). Linking a provider, whose request is read against the
//! providers' clients, is in `providers.rs` (AUTH-087).

use std::sync::Arc;

use axum::{
    extract::{RawPathParams, State},
    http::StatusCode,
};
use platform::{
    authn::Principal,
    errors::{Problem, RequestError},
    http::{FieldErrors, Json, Routes, path_id},
    rate_limit::{LimitError, RateLimits},
    types::Client,
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use crate::modules::auth::{
    error::AuthError,
    events::{IDENTITY_ADDED, IDENTITY_REMOVED},
    model::{Identities, Password},
    service::Auth,
};

/// The routes of the caller's identities, which take the service as their state.
pub(super) fn routes() -> Routes<Arc<Auth>> {
    Routes::new()
        .events(&[&IDENTITY_ADDED, &IDENTITY_REMOVED])
        .user(routes!(identities), &[])
        .user(
            routes!(add_password),
            &[
                &RequestError::ReauthenticationRequired,
                &AuthError::PasswordAlreadySet,
                &LimitError::RateLimited,
            ],
        )
        .user(
            routes!(remove_identity),
            &[
                &RequestError::ReauthenticationRequired,
                &AuthError::IdentityNotFound,
                &AuthError::LastIdentity,
                &LimitError::RateLimited,
            ],
        )
}

/// Reads the ways the caller's account signs in: its password, and its accounts at providers.
#[utoipa::path(
    get,
    path = "/v1/users/me/identities",
    tag = "auth",
    responses(
        (status = 200, description = "The caller's identities.", body = Identities),
    )
)]
async fn identities(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
) -> Result<axum::Json<Identities>, Problem<AuthError>> {
    Ok(axum::Json(auth.identities(&client, &principal).await?))
}

/// What adding a password takes (AUTH-086).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddPasswordRequest {
    /// The account's password (AUTH-003).
    #[schema(value_type = Password)]
    password: String,
}

impl TryFrom<AddPasswordRequest> for Password {
    type Error = RequestError;

    fn try_from(request: AddPasswordRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let password = errors.check("/password", Self::parse(request.password));
        errors.finish(password)
    }
}

/// Adds a password to the caller's account, which has none, once the session has proved who its
/// user is within the last 5 minutes. An account that has one changes it with the current one.
#[utoipa::path(
    post,
    path = "/v1/users/me/identities/password",
    tag = "auth",
    request_body = AddPasswordRequest,
    responses(
        (status = 200, description = "The caller's identities, the password among them.", body = Identities),
    )
)]
async fn add_password(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<AddPasswordRequest>,
) -> Result<axum::Json<Identities>, Problem<AuthError>> {
    let password = Password::try_from(request).map_err(AuthError::from)?;
    let identities = auth
        .add_password(&limits, &client, &principal, password)
        .await?;
    Ok(axum::Json(identities))
}

/// Removes one of the caller's identities, their password or an account at a provider, unless it
/// is their last, once the session has proved who its user is within the last 5 minutes, and
/// ends their other sessions that signed in with it.
#[utoipa::path(
    delete,
    path = "/v1/users/me/identities/{identity_id}",
    tag = "auth",
    params(
        ("identity_id" = uuid::Uuid, Path, description = "The identity."),
    ),
    responses(
        (status = 204, description = "The identity is removed, and every other session that signed in with it ended."),
    )
)]
async fn remove_identity(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    params: RawPathParams,
) -> Result<StatusCode, Problem<AuthError>> {
    let id = path_id(&params, "identity_id");
    auth.remove_identity(&limits, &client, &principal, id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
