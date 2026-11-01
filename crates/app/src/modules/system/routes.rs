//! The capabilities endpoint (SYSTEM-001).

use axum::{Json, extract::State};
use platform::http::Routes;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

/// The features this deployment offers.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Capabilities {
    /// One member per feature a deployment can turn off, `true` while the feature is on. A
    /// feature this API doesn't have is absent.
    features: Features,
}

/// Each feature a deployment can turn off, and whether it is on.
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub(crate) struct Features {
    /// Whether the deployment sends email (MAIL-004). While it is off, a client hides what
    /// needs email, such as a way to reset a password.
    pub(crate) email: bool,
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
