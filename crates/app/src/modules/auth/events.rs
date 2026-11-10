//! The events `auth` publishes, each with its payload, which the event catalog documents
//! (API-19, REALTIME-008).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its events here (the platform spec's events)"
)]

use platform::{events::UserEvent, types::SessionId};
use serde::Serialize;
use utoipa::ToSchema;

/// One of the user's sessions started, sent to their own streams (AUTH-046).
pub(super) const SESSION_CREATED: UserEvent<SessionChanged> = UserEvent::new("session.created");

/// One of the user's sessions ended, sent to their own streams (AUTH-046).
pub(super) const SESSION_ENDED: UserEvent<SessionChanged> = UserEvent::new("session.ended");

/// The payload of `session.created` and `session.ended`: the session, which a client finds in
/// `GET /v1/users/me/sessions` while it lasts, since a device's name is personal data (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct SessionChanged {
    /// The session.
    pub(super) session_id: SessionId,
}
