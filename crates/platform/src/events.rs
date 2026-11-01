//! Access-change signals: the notice, at commit, that a user lost access, so the real-time hub
//! closes or re-filters their streams (D35, SEC-06). The event log and the hub that listens for
//! the signals arrive with step 2.4.

use serde::Serialize;

use crate::{
    db::UnitOfWork,
    errors::Internal,
    types::{SessionId, UserId},
};

/// The channel the signals go out on.
const CHANNEL: &str = "access_changed";

/// A change to what a user may access. It holds IDs only, and is never stored (D35).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccessChange {
    /// A session ended, by signing out or by the reuse of a refresh token, so its streams close.
    SessionEnded {
        /// The session's user.
        user_id: UserId,
        /// The session.
        session_id: SessionId,
    },
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
