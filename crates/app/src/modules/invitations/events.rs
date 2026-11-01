//! The events `invitations` publishes (API-19, the platform spec's events).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its events here (the platform spec's events)"
)]

use platform::events::{UserEvent, WorkspaceEvent};

use crate::modules::access::MembersRead;

/// An invitation was created or refreshed for the user's address, sent to their streams
/// (INVITATIONS-008).
pub(super) const INVITATION_RECEIVED: UserEvent = UserEvent::new("invitation.received");
/// Someone joined the workspace by accepting an invitation, sent to the members who may list
/// members (INVITATIONS-005).
pub(super) const MEMBER_JOINED: WorkspaceEvent<MembersRead> = WorkspaceEvent::new("member.joined");
