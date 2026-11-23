//! Routes that only the tests serve, registered the way a module registers its own, for the
//! behaviors of the pipeline that no route of the API shows, or that only the routes of a part of
//! the template show, which a fork may not keep: a request that waits, one that never ends, a
//! panic, an internal error, an insert that a foreign key refuses, a keyed operation that
//! publishes an event, a rank refusal after a write, bytes counted in a budget, and an email sent
//! (TEST-04).

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{Extension, Json, extract::State, http::StatusCode};
use platform::{
    authn::Principal,
    authz::{self, Authorize, Authorizer, Permission},
    errors::{Internal, PlatformError, Problem},
    events::UserEvent,
    http::{Idempotent, Json as Body, Routes},
    idempotency::Claimed,
    mail::{Email, Mail},
    rate_limit::{Bucket, LimitError, RateLimits},
    types::{Client, EmailAddress, UserId, WorkspaceId},
};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::{Api, Options};

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

/// What `POST /v1/keyed` takes.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct KeyedRequest {
    /// A value the answer echoes.
    value: String,
    /// Whether the operation fails after it published its event, so its unit of work rolls back.
    #[serde(default)]
    fail: bool,
    /// Whether the operation publishes its event before it claims the key, a claim that follows
    /// another statement of its unit of work (PLATFORM-036).
    #[serde(default)]
    late: bool,
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
const KEYED_RAN: UserEvent<Ran> = UserEvent::new("keyed.ran");

/// The payload of `keyed.ran`, which holds nothing.
#[derive(Serialize, ToSchema)]
struct Ran {}

/// A keyed operation, as a module's keyed route runs one: it claims the key, publishes an event
/// to its caller, and commits with its result (PLATFORM-026, PLATFORM-036). A `late` one
/// publishes the event before it claims the key, which the claim then refuses.
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
        .begin(&client, Some(&principal))
        .await
        .map_err(PlatformError::from)?;
    if request.body.late {
        uow.publish_to_user(&KEYED_RAN, principal.user_id(), &Ran {})
            .await
            .map_err(PlatformError::from)?;
    }
    if let Claimed::Replay(keyed) = uow.claim(principal.user_id(), request.key.as_ref()).await? {
        return Ok(Json(keyed));
    }
    uow.publish_to_user(&KEYED_RAN, principal.user_id(), &Ran {})
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

/// Publishes an event to a user that no row holds, as its caller, so that the insert fails on a
/// foreign key while the caller's session lasts (PLATFORM-097).
#[utoipa::path(
    post,
    path = "/v1/orphan",
    responses((status = 204, description = "Never.")),
)]
async fn orphan(
    Extension(authorizer): Extension<Authorizer>,
    client: Client,
    principal: Principal,
) -> Result<StatusCode, Problem<PlatformError>> {
    let mut uow = authorizer
        .database
        .begin(&client, Some(&principal))
        .await
        .map_err(PlatformError::from)?;
    let nobody = UserId::from_uuid(uuid::Uuid::nil());
    uow.publish_to_user(&KEYED_RAN, nobody, &Ran {})
        .await
        .map_err(PlatformError::from)?;
    uow.commit().await.map_err(PlatformError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The budget of `POST /v1/spend`: 1,000 bytes for each user, in each window of a minute.
pub const SPENDING: Bucket = Bucket {
    name: "spending",
    limit: 1_000,
    window: Duration::from_secs(60),
};

/// What `POST /v1/spend` takes.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct Spent {
    /// How many bytes the request counts.
    bytes: u32,
}

/// Counts `bytes` in its caller's budget of the tests' own, as a module's route counts the bytes
/// of a request (PLATFORM-089).
#[utoipa::path(
    post,
    path = "/v1/spend",
    request_body = Spent,
    responses((status = 204, description = "The budget took the bytes.")),
)]
async fn spend(
    principal: Principal,
    limits: RateLimits,
    Body(spent): Body<Spent>,
) -> Result<StatusCode, Problem<LimitError>> {
    let user = principal.user_id();
    limits
        .spend(SPENDING, user.uuid().as_bytes(), spent.bytes)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What `POST /v1/mail` takes.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct Mailed {
    /// The address the email goes to.
    to: String,
    /// Its subject.
    subject: String,
}

/// Sends an email of the tests' own to `to`, as a module's operation sends one: in its unit of
/// work, under its caller's scope, through `mail` (PLATFORM-021).
#[utoipa::path(
    post,
    path = "/v1/mail",
    request_body = Mailed,
    responses((status = 204, description = "The email is enqueued.")),
)]
async fn send(
    State(mail): State<Mail>,
    Extension(authorizer): Extension<Authorizer>,
    client: Client,
    principal: Principal,
    Body(mailed): Body<Mailed>,
) -> Result<StatusCode, Problem<PlatformError>> {
    let mut uow = authorizer
        .database
        .begin(&client, Some(&principal))
        .await
        .map_err(PlatformError::from)?;
    let email = fixture_email(mailed)?;
    mail.send(&mut uow, principal.job_scope(), email)
        .await
        .map_err(PlatformError::from)?;
    uow.commit().await.map_err(PlatformError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// What the workspace route of the fixtures requires: reading the workspace, which every role
/// holds, as `access`'s matrix names it.
struct Reading;

impl Permission for Reading {
    const NAME: &'static str = "workspace.read";
}

/// Sends an email of the tests' own to `to`, as a workspace's operation sends one: in the unit of
/// work its proof opens, under the member's scope in the workspace (PLATFORM-013).
#[utoipa::path(
    post,
    path = "/v1/workspaces/{workspace_id}/mail",
    params(("workspace_id" = WorkspaceId, Path, description = "The workspace.")),
    request_body = Mailed,
    responses((status = 204, description = "The email is enqueued.")),
)]
async fn send_in_workspace(
    State(mail): State<Mail>,
    authorize: Authorize<Reading>,
    Body(mailed): Body<Mailed>,
) -> Result<StatusCode, Problem<PlatformError>> {
    let (mut uow, authorized) = authorize.open().await?;
    let email = fixture_email(mailed)?;
    mail.send(&mut uow, authorized.job_scope(), email)
        .await
        .map_err(PlatformError::from)?;
    uow.commit().await.map_err(PlatformError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The email of the tests' own that `mailed` asks for.
fn fixture_email(mailed: Mailed) -> Result<Email, PlatformError> {
    Ok(Email {
        to: EmailAddress::parse(&mailed.to)
            .map_err(|_| PlatformError::from(Internal::new("the fixture's address is invalid")))?,
        text: format!("The plain-text part of {}.", mailed.subject),
        html: format!("<p>The HTML part of {}.</p>", mailed.subject),
        subject: mailed.subject,
    })
}

/// Publishes an event to its caller, then refuses them as an operation refuses a member who
/// outranks the caller, so that the refusal finds a write to roll back, which no route of the
/// API makes before such a refusal (PLATFORM-010).
#[utoipa::path(
    post,
    path = "/v1/workspaces/{workspace_id}/outranked",
    params(("workspace_id" = WorkspaceId, Path, description = "The workspace.")),
    responses((status = 204, description = "Never.")),
)]
async fn outranked(authorize: Authorize<Reading>) -> Result<StatusCode, Problem<PlatformError>> {
    let (mut uow, authorized) = authorize.open().await?;
    uow.publish_to_user(&KEYED_RAN, authorized.principal().user_id(), &Ran {})
        .await
        .map_err(PlatformError::from)?;
    Err(authz::refuse(uow, &authorized).await.into())
}

/// The routes `POST /v1/mail` and `POST /v1/workspaces/{workspace_id}/mail`, which send their
/// emails through `mail`, built from the settings of the API they are added to.
pub fn mailing(mail: Mail) -> Routes {
    Routes::new()
        .user(routes!(send), &[])
        .merge(Routes::new().workspace::<Reading>(routes!(send_in_workspace), &[]))
        .with_state(mail)
}

/// The API, with the fixtures' routes added, whose gate no test holds.
pub async fn api() -> Api {
    Api::with(Options::routes(routes(Arc::default()))).await
}

/// The fixtures' routes, with `held` behind `GET /v1/gate`, which the tests add to the API's.
pub fn routes(held: Arc<Gate>) -> Routes {
    let routes = Routes::new()
        .public(routes!(panics), &[])
        .merge(Routes::new().public(routes!(broken), &[]))
        .merge(Routes::new().public(routes!(gate), &[]).with_state(held));
    let routes = routes.merge(Routes::new().user(routes!(spend), &[]));
    let routes = routes.merge(Routes::new().user(routes!(orphan), &[]));
    let routes = routes.merge(Routes::new().workspace::<Reading>(routes!(outranked), &[]));
    routes.merge(
        Routes::new()
            .events(&[&KEYED_RAN])
            .user(routes!(keyed), &[])
            .with_state(Arc::new(AtomicU64::new(0))),
    )
}
