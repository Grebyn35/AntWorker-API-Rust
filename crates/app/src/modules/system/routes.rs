//! The capabilities endpoint (SYSTEM-001).

use axum::Json;
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

/// No feature can be turned off yet.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct Features {}

/// The module's routes.
pub(crate) fn routes() -> Routes {
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
async fn capabilities() -> Json<Capabilities> {
    Json(Capabilities {
        features: Features {},
    })
}
