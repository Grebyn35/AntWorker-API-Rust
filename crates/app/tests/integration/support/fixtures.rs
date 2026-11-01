//! Routes that only the tests serve, registered the way a module registers its own, for the
//! behaviors of the pipeline that no route of the API shows, or that only an optional part's
//! routes show: a request that waits, one that never ends, a panic, an internal error, a
//! documented query parameter, and a keyed operation that publishes an event.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
};
use platform::{
    authn::Principal,
    authz::Authorizer,
    errors::{Internal, PlatformError, Problem},
    events::UserEvent,
    http::{Idempotent, Routes},
    idempotency::Claimed,
    types::Client,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Notify;
use utoipa::{IntoParams, ToSchema};
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

/// What `POST /v1/keyed` takes.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct KeyedRequest {
    /// A value the answer echoes.
    value: String,
    /// Whether the operation fails after it published its event, so its unit of work rolls back.
    #[serde(default)]
    fail: bool,
}

/// What `POST /v1/keyed` answers: the value, and which fresh run of the route answered it.
#[derive(Deserialize, Serialize, ToSchema)]
struct Keyed {
    /// The request's value.
    value: String,
    /// How many fresh runs the route had made when it answered this one, from 1.
    run: u64,
}

/// The event that `POST /v1/keyed` publishes to its caller.
#[expect(
    clippy::disallowed_methods,
    reason = "a test-only route declares its event where it publishes it"
)]
const KEYED_RAN: UserEvent = UserEvent::new("keyed.ran");

/// A keyed operation, as a module's keyed route runs one: it claims the key, publishes an event
/// to its caller, and commits with its result (PLATFORM-015, PLATFORM-018).
#[utoipa::path(
    post,
    path = "/v1/keyed",
    request_body = KeyedRequest,
    params(("Idempotency-Key" = Option<String>, Header, description = "The caller's key (API-12).")),
    responses((status = 200, description = "The value, and the run that answered it.", body = Keyed)),
)]
async fn keyed(
    State(runs): State<Arc<AtomicU64>>,
    Extension(authorizer): Extension<Authorizer>,
    client: Client,
    principal: Principal,
    request: Idempotent<KeyedRequest>,
) -> Result<Json<Keyed>, Problem<PlatformError>> {
    let mut uow = authorizer
        .database
        .begin(&client)
        .await
        .map_err(PlatformError::from)?;
    if let Claimed::Replay(keyed) = uow.claim(principal.user_id(), request.key.as_ref()).await? {
        return Ok(Json(keyed));
    }
    uow.publish_to_user(&KEYED_RAN, principal.user_id(), &json!({}))
        .await
        .map_err(PlatformError::from)?;
    if request.body.fail {
        return Err(PlatformError::from(Internal::new("the fixture failed")).into());
    }
    let keyed = Keyed {
        value: request.body.value,
        run: runs.fetch_add(1, Ordering::SeqCst) + 1,
    };
    uow.commit_with(&keyed).await.map_err(PlatformError::from)?;
    Ok(Json(keyed))
}

/// The fixtures' routes, with `held` behind `GET /v1/gate`, which the tests add to the API's.
pub fn routes(held: Arc<Gate>) -> Routes {
    Routes::new()
        .public(routes!(panics), &[])
        .merge(Routes::new().public(routes!(broken), &[]))
        .merge(Routes::new().public(routes!(items), &[]))
        .merge(Routes::new().public(routes!(gate), &[]).with_state(held))
        .merge(
            Routes::new()
                .user(routes!(keyed), &[])
                .with_state(Arc::new(AtomicU64::new(0))),
        )
}
