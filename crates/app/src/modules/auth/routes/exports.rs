//! Requesting an export of the caller's data, which `auth` serves, since it first checks that the
//! caller's session proved who they are just before, as it does for a deletion of the account,
//! and `accounts` then builds (the auth spec's Routes, ACCOUNTS-020, D137).

use std::sync::Arc;

use axum::extract::State;
use platform::{
    authn::Principal,
    errors::Problem,
    http::{ResourceResponse, Routes},
    rate_limit::{LimitError, RateLimits},
    types::Client,
};
use utoipa_axum::routes;

use crate::modules::{
    accounts::{Export, Exports},
    auth::{error::AuthError, service::Auth},
};

/// The state of the route that requests an export: the service that checks the caller's proof,
/// and the exports of `accounts`, which insert the export once it has.
#[derive(Clone)]
pub(crate) struct ExportRequests {
    auth: Arc<Auth>,
    exports: Exports,
}

impl ExportRequests {
    /// The state of `auth`, which checks the proof, and `exports`, which `accounts` builds.
    pub(crate) fn new(auth: Arc<Auth>, exports: Exports) -> Self {
        Self { auth, exports }
    }
}

/// The route that requests an export (ACCOUNTS-020).
pub(crate) fn export_routes() -> Routes<ExportRequests> {
    Routes::new().user(
        routes!(request_export),
        &[
            &AuthError::ReauthenticationRequired,
            &LimitError::RateLimited,
        ],
    )
}

/// Requests an export of the caller's data, whose archive a job builds, once the session that
/// authenticates the request has proved who its user is within the last 5 minutes, since the
/// archive holds everything the user keeps.
#[utoipa::path(
    post,
    path = "/v1/users/me/exports",
    tag = "auth",
    responses(
        (status = 202, description = "The export, pending until its archive is built.", body = Export,
            headers(("location" = String, description = "The export: `/v1/users/me/exports/{id}`."))),
    )
)]
async fn request_export(
    State(state): State<ExportRequests>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
) -> Result<ResourceResponse<Export>, Problem<AuthError>> {
    let export = state
        .auth
        .request_export(&state.exports, &limits, &client, &principal)
        .await?;
    let location = format!("/v1/users/me/exports/{}", export.id);
    Ok(ResourceResponse::accepted(export, location))
}
