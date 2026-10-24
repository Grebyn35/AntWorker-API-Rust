//! Routes that only the tests serve, registered the way a module registers its own, for the
//! behaviors of the pipeline that no route of the API shows: a request that waits, one that
//! never ends, a panic, an internal error, and a documented query parameter.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
};
use platform::{
    errors::{Internal, Problem},
    http::Routes,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Notify;
use utoipa::IntoParams;
use utoipa_axum::routes;

/// Holds `GET /v1/gate` until the test releases it.
#[derive(Default)]
pub struct Gate {
    /// Notified when a request enters the gate.
    pub entered: Notify,
    /// Lets the request in the gate finish.
    pub release: Notify,
}

/// Waits until the test releases it.
#[utoipa::path(get, path = "/v1/gate", responses((status = 204, description = "Released.")))]
async fn gate(State(gate): State<Arc<Gate>>) -> StatusCode {
    let released = gate.release.notified();
    gate.entered.notify_one();
    released.await;
    StatusCode::NO_CONTENT
}

/// Never answers.
#[utoipa::path(get, path = "/v1/forever", responses((status = 204, description = "Never.")))]
async fn forever() -> StatusCode {
    std::future::pending::<()>().await;
    StatusCode::NO_CONTENT
}

/// Panics.
#[utoipa::path(get, path = "/v1/panic", responses((status = 204, description = "Never.")))]
#[expect(clippy::panic, reason = "the fixture proves the catch layer (ERR-12)")]
async fn panics() -> StatusCode {
    panic!("the fixture panicked");
}

/// Fails with an internal error whose cause is known.
#[utoipa::path(get, path = "/v1/broken", responses((status = 204, description = "Never.")))]
async fn broken() -> Result<StatusCode, Problem<Internal>> {
    Err(Internal::new(std::io::Error::other("the disk is gone")).into())
}

/// The query of `GET /v1/items`.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
struct Page {
    /// How many items.
    limit: Option<u32>,
}

/// Takes one query parameter.
#[utoipa::path(
    get,
    path = "/v1/items",
    params(Page),
    responses((status = 200, description = "The items.", body = Value)),
)]
async fn items(Query(page): Query<Page>) -> Json<Value> {
    Json(json!({ "limit": page.limit }))
}

/// The API's routes and the fixtures, with `held` behind `GET /v1/gate`.
pub fn routes(held: Arc<Gate>) -> Routes {
    app::routes()
        .merge(Routes::new().public(routes!(forever)))
        .merge(Routes::new().public(routes!(panics)))
        .merge(Routes::new().public(routes!(broken)))
        .merge(Routes::new().public(routes!(items)))
        .merge(Routes::new().public(routes!(gate)).with_state(held))
}
