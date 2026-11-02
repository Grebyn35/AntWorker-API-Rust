//! Events: publishing them inside a unit of work, as a row of `events` that its commit positions
//! and notifies (D1, DATA-27, PLATFORM-071); access-change signals, the notice, at commit, that a
//! user's access changed, so that their streams close (D35, SEC-06); and the real-time hub, which
//! delivers what commits to the streams allowed to receive it (SEC-17).

mod hub;
mod log;
mod streams;
mod subscription;

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::{
    authz::Permission,
    db::UnitOfWork,
    errors::Internal,
    telemetry,
    types::{SessionId, UserId, WorkspaceId},
};

pub use hub::{Hub, SubscribeError};
pub use subscription::{Closed, Delivery, Event, Subscription};

/// The channel the signals go out on.
const CHANNEL: &str = "access_changed";

/// The channel on which each commit that published names the last position it gave.
pub(crate) const EVENTS_CHANNEL: &str = "events";

/// The key of the transaction-level advisory lock under which a commit positions its events
/// (PLATFORM-071): the bytes of `events`.
pub(crate) const POSITION_LOCK: i64 = 0x6576_656e_7473;

/// An event that goes to one user's streams, such as `membership.created`. Each module declares
/// its own in its `events.rs`, the one place clippy lets build one.
#[derive(Clone, Copy, Debug)]
pub struct UserEvent {
    name: &'static str,
}

impl UserEvent {
    /// The user event `name`, `<resource>.<past-tense verb>` (API-19).
    pub const fn new(name: &'static str) -> Self {
        Self { name }
    }
}

/// An event that goes to the streams of a workspace's members who hold permission `P`, such as
/// `member.removed` (SEC-17). Each module declares its own in its `events.rs`, the one place
/// clippy lets build one.
#[derive(Debug)]
pub struct WorkspaceEvent<P> {
    name: &'static str,
    permission: PhantomData<fn() -> P>,
}

impl<P: Permission> WorkspaceEvent<P> {
    /// The workspace event `name`, `<resource>.<past-tense verb>` (API-19).
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            permission: PhantomData,
        }
    }
}

impl UnitOfWork {
    /// Publishes `event` to `user`'s streams with `payload`, which holds IDs only (API-19), when
    /// the unit of work commits, and never if it rolls back (PLATFORM-015). Its commit gives the
    /// event its position (PLATFORM-071).
    pub async fn publish_to_user(
        &mut self,
        event: &UserEvent,
        user: UserId,
        payload: &impl Serialize,
    ) -> Result<(), Internal> {
        self.publish(event.name, Some(user), None, None, payload)
            .await
    }

    /// Publishes `event` to the streams of `workspace`'s members who hold its permission, with
    /// `payload`, which holds IDs only (API-19), when the unit of work commits, and never if it
    /// rolls back (PLATFORM-015).
    pub async fn publish_to_workspace<P: Permission>(
        &mut self,
        event: &WorkspaceEvent<P>,
        workspace: WorkspaceId,
        payload: &impl Serialize,
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
            "INSERT INTO events (scope, user_id, workspace_id, type, permission, payload, traceparent)
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
    /// A user's membership in a workspace started or ended, so their streams close, and open
    /// again with their memberships as they then stand (PLATFORM-017).
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

/// Sends `change` on the channel when `uow` commits, and never if it rolls back: Postgres delivers
/// a notification only at commit (D35).
pub async fn signal(uow: &mut UnitOfWork, change: AccessChange) -> Result<(), Internal> {
    let payload = serde_json::to_string(&change).map_err(Internal::new)?;
    sqlx::query!("SELECT pg_notify($1, $2)", CHANNEL, payload)
        .execute(uow.executor())
        .await?;
    Ok(())
}
