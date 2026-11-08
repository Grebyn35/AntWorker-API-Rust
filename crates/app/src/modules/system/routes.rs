//! The capabilities endpoint (SYSTEM-001).

use axum::{Json, extract::State};
use platform::http::Routes;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

/// The features this deployment offers.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Capabilities {
    /// One member per feature a deployment can turn off, `true` while the feature is on, and one
    /// per optional part that changes what a client offers, `true` while the fork keeps it. A
    /// feature or a part this API doesn't have is absent.
    features: Features,
}

/// Each feature a deployment can turn off, and whether it is on, and each optional part that the
/// fork keeps and that changes what a client offers (D118).
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub(crate) struct Features {
    /// Whether the deployment sends email (PLATFORM-022). While it is off, a client hides what
    /// needs email, such as a way to reset a password.
    email: bool,
    // part: invitations
    /// The fork keeps `invitations`, so a workspace's members invite others.
    invitations: bool,
    // end part: invitations
    // part: files
    /// The fork keeps `files`, so users upload files, avatars and workspace pictures.
    files: bool,
    // end part: files
}

impl Features {
    /// The features of a deployment that sends email while `email` is on, with every optional
    /// part the fork keeps.
    pub(crate) fn new(email: bool) -> Self {
        Self {
            email,
            // part: invitations
            invitations: true,
            // end part: invitations
            // part: files
            files: true,
            // end part: files
        }
    }
}

/// The module's routes, which take the deployment's features.
pub(crate) fn routes() -> Routes<Features> {
    Routes::new().public(routes!(capabilities), &[])
}

/// Lists the features a deployment can turn off, and whether each one is on.
#[utoipa::path(
    get,
    path = "/v1/capabilities",
    tag = "system",
    responses(
        (status = 200, description = "The deployment's features.", body = Capabilities),
    )
)]
async fn capabilities(State(features): State<Features>) -> Json<Capabilities> {
    Json(Capabilities { features })
}
