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

use crate::modules::access::{InvitationsRead, MembersRead};

/// An invitation was created for the user's address, refreshed, or sent again, each of which
/// sends its email, sent to their streams (INVITATIONS-008).
pub(super) const INVITATION_RECEIVED: UserEvent<AddressedInvitation> =
    UserEvent::new("invitation.received");
/// The role of an invitation to the user's address changed, sent to their streams
/// (INVITATIONS-013).
pub(super) const INVITATION_REVISED: UserEvent<AddressedInvitation> =
    UserEvent::new("invitation.revised");
/// An invitation to the user's address was cancelled, sent to their streams (INVITATIONS-016).
pub(super) const INVITATION_WITHDRAWN: UserEvent<AddressedInvitation> =
    UserEvent::new("invitation.withdrawn");
/// A member invited someone, sent to the members who may read the workspace's invitations
/// (INVITATIONS-001).
pub(super) const INVITATION_CREATED: WorkspaceEvent<InvitationsRead, InvitationChanged> =
    WorkspaceEvent::new("invitation.created");
/// An invitation of the workspace was refreshed, its role changed, or it was sent again, sent to
/// the members who may read the workspace's invitations (INVITATIONS-003, INVITATIONS-013,
/// INVITATIONS-015).
pub(super) const INVITATION_UPDATED: WorkspaceEvent<InvitationsRead, InvitationChanged> =
    WorkspaceEvent::new("invitation.updated");
/// A member cancelled an invitation of the workspace, sent to the members who may read its
/// invitations (INVITATIONS-016).
pub(super) const INVITATION_CANCELLED: WorkspaceEvent<InvitationsRead, InvitationChanged> =
    WorkspaceEvent::new("invitation.cancelled");
/// Its invitee declined an invitation of the workspace, sent to the members who may read its
/// invitations (INVITATIONS-017).
pub(super) const INVITATION_DECLINED: WorkspaceEvent<InvitationsRead, InvitationChanged> =
    WorkspaceEvent::new("invitation.declined");
/// Someone joined the workspace by accepting an invitation, sent to the members who may list
/// members (INVITATIONS-005).
pub(super) const MEMBER_JOINED: WorkspaceEvent<MembersRead, MemberJoined> =
    WorkspaceEvent::new("member.joined");

/// The payload of the events that reach an invitee: an invitation to the user's address, which
/// they read again in their list of invitations.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct AddressedInvitation {
    /// The invitation.
    pub(super) invitation_id: uuid::Uuid,
    /// The workspace it invites them to.
    pub(super) workspace_id: WorkspaceId,
}

/// The payload of the events about one of the workspace's invitations, which its members read
/// again.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct InvitationChanged {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The invitation.
    pub(super) invitation_id: uuid::Uuid,
}

/// The payload of `member.joined`: someone joined the workspace by accepting an invitation.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct MemberJoined {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The member who joined.
    pub(super) user_id: UserId,
}
