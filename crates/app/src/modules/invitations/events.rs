//! The events `invitations` publishes, each with its payload, which the event catalog documents
//! (API-19, REALTIME-008).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its events here (the platform spec's events)"
)]

use platform::{
    events::{UserEvent, WorkspaceEvent},
    types::{UserId, WorkspaceId},
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::modules::access::MembersRead;

/// An invitation was created or refreshed for the user's address, sent to their streams
/// (INVITATIONS-008).
pub(super) const INVITATION_RECEIVED: UserEvent<InvitationReceived> =
    UserEvent::new("invitation.received");
/// Someone joined the workspace by accepting an invitation, sent to the members who may list
/// members (INVITATIONS-005).
pub(super) const MEMBER_JOINED: WorkspaceEvent<MembersRead, MemberJoined> =
    WorkspaceEvent::new("member.joined");

/// The payload of `invitation.received`: an invitation for the user's address.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct InvitationReceived {
    /// The invitation, which the user may accept.
    pub(super) invitation_id: uuid::Uuid,
    /// The workspace it invites them to.
    pub(super) workspace_id: WorkspaceId,
}

/// The payload of `member.joined`: someone joined the workspace by accepting an invitation.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct MemberJoined {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The member who joined.
    pub(super) user_id: UserId,
}
