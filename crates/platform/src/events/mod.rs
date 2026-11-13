//! Events: publishing them inside a unit of work, as a row of `events` that its commit positions
//! and notifies (D1, DATA-22, PLATFORM-028); access-change signals, the notice, at commit, that a
//! user's access changed, so that the streams it affects close (D35, SEC-06, PLATFORM-031); the
//! real-time hub, which
//! delivers what commits to the streams allowed to receive it (SEC-19); and the event catalog,
//! which documents each event's payload (REALTIME-008).

pub(crate) mod catalog;
mod hub;
mod log;
mod streams;
mod subscription;
mod task;

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
    authz::Permission,
    db::UnitOfWork,
    errors::Internal,
    telemetry,
    types::{SessionId, UserId, WorkspaceId},
};

pub use catalog::{Catalogued, Entry, FrameId, StreamFrame};
pub use hub::{Hub, ListenError, Listener, SubscribeError, listen};
pub use subscription::{Closed, Delivery, Event, Subscription};

/// The channel the signals go out on.
const CHANNEL: &str = "access_changed";

/// The channel on which each commit that published names the last position it gave.
pub(crate) const EVENTS_CHANNEL: &str = "events";

/// The key of the transaction-level advisory lock under which a commit positions its events
/// (PLATFORM-028): the bytes of `events`.
pub(crate) const POSITION_LOCK: i64 = 0x6576_656e_7473;

/// An event that goes to one user's streams, such as `membership.created`, with a payload of
/// type `T`, which the event catalog documents (API-19, REALTIME-008). Each module declares its
/// own in its `events.rs`, the one place clippy lets build one.
#[derive(Debug)]
pub struct UserEvent<T> {
    name: &'static str,
    payload: PhantomData<fn(&T)>,
}

impl<T: Serialize + ToSchema> UserEvent<T> {
    /// The user event `name`, `<resource>.<past-tense verb>` (API-19).
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            payload: PhantomData,
        }
    }
}

/// An event that goes to the streams of a workspace's members who hold permission `P`, such as
/// `member.removed` (SEC-19), with a payload of type `T`, which the event catalog documents
/// (API-19, REALTIME-008). Each module declares its own in its `events.rs`, the one place clippy
/// lets build one.
#[derive(Debug)]
pub struct WorkspaceEvent<P, T> {
    name: &'static str,
    permission: PhantomData<fn() -> P>,
    payload: PhantomData<fn(&T)>,
}

impl<P: Permission, T: Serialize + ToSchema> WorkspaceEvent<P, T> {
    /// The workspace event `name`, `<resource>.<past-tense verb>` (API-19).
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            permission: PhantomData,
            payload: PhantomData,
        }
    }
}

impl UnitOfWork {
    /// Publishes `event` to `user`'s streams with `payload`, of the type the event declares
    /// (API-19), when the unit of work commits, and never if it rolls back (PLATFORM-026). Its
    /// commit gives the event its position (PLATFORM-028).
    pub async fn publish_to_user<T: Serialize + ToSchema>(
        &mut self,
        event: &UserEvent<T>,
        user: UserId,
        payload: &T,
    ) -> Result<(), Internal> {
        self.publish(event.name, Some(user), None, None, payload)
            .await
    }

    /// Publishes `event` to the streams of `workspace`'s members who hold its permission, with
    /// `payload`, of the type the event declares (API-19), when the unit of work commits, and
    /// never if it rolls back (PLATFORM-026).
    pub async fn publish_to_workspace<P: Permission, T: Serialize + ToSchema>(
        &mut self,
        event: &WorkspaceEvent<P, T>,
        workspace: WorkspaceId,
        payload: &T,
    ) -> Result<(), Internal> {
        self.publish(event.name, None, Some(workspace), Some(P::NAME), payload)
            .await
    }

    async fn publish(
        &mut self,
        name: &'static str,
        user: Option<UserId>,
        workspace: Option<WorkspaceId>,
        permission: Option<&'static str>,
        payload: &impl Serialize,
    ) -> Result<(), Internal> {
        let payload = serde_json::to_value(payload).map_err(Internal::new)?;
        let scope = if workspace.is_some() {
            "workspace"
        } else {
            "user"
        };
        // The event carries its trace context, so its delivery joins the trace that published it
        // (OBS-07).
        let traceparent = telemetry::traceparent(&tracing::Span::current());
        let id = sqlx::query_scalar!(
            "INSERT INTO events (scope, user_id, workspace_id, name, permission, payload, traceparent)
            VALUES ($1::text::event_scope, $2, $3, $4, $5, $6, $7)
            RETURNING id",
            scope,
            user.map(UserId::uuid),
            workspace.map(WorkspaceId::uuid),
            name,
            permission,
            payload,
            traceparent,
        )
        .fetch_one(self.executor())
        .await?;
        self.published(id);
        Ok(())
    }
}

/// A change to what a user may access. It holds IDs only, and is never stored (D35).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccessChange {
    /// A session ended, by signing out or by the reuse of a refresh token, so its streams close.
    SessionEnded {
        /// The session's user.
        user_id: UserId,
        /// The session.
        session_id: SessionId,
    },
    /// A user's membership in a workspace started, ended or changed its role, so their streams
    /// whose memberships don't hold the change close, and open again with their memberships as
    /// they then stand (PLATFORM-031).
    MembershipChanged {
        /// The user.
        user_id: UserId,
        /// The workspace.
        workspace_id: WorkspaceId,
    },
}

impl AccessChange {
    /// The user whose access changed.
    fn user_id(&self) -> UserId {
        match *self {
            Self::SessionEnded { user_id, .. } | Self::MembershipChanged { user_id, .. } => user_id,
        }
    }
}

/// A signal, as the hub receives it: the change, and the ID of the transaction that made it, as
/// `pg_current_xact_id()` gives it, which tells the hub whether a stream's memberships hold the
/// change (PLATFORM-031). A replica of the release before sends none while a deploy runs both
/// (DATA-10), and its signal closes every stream it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
struct Signal {
    #[serde(flatten)]
    change: AccessChange,
    #[serde(default)]
    transaction: Option<u64>,
}

/// Sends each of `changes` on the channel, with the ID of `uow`'s transaction, when `uow`
/// commits, and never if it rolls back: Postgres delivers a notification only at commit (D35).
/// One statement sends them all, so an operation that changes many users' access, such as a
/// workspace's deletion, sends as many signals in one round trip.
pub async fn signal(uow: &mut UnitOfWork, changes: &[AccessChange]) -> Result<(), Internal> {
    if changes.is_empty() {
        return Ok(());
    }
    let payloads = changes
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(Internal::new)?;
    sqlx::query!(
        "SELECT pg_notify($1, (payload
            || jsonb_build_object('transaction', pg_current_xact_id()::text::numeric))::text)
        FROM unnest($2::jsonb[]) AS payload",
        CHANNEL,
        &payloads,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}
