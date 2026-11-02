//! The events `workspaces` publishes, each with its payload, which the event catalog documents
//! (API-19, REALTIME-008).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its events here (the platform spec's events)"
)]

use platform::{
    events::{UserEvent, WorkspaceEvent},
    types::{SessionId, UserId, WorkspaceId},
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::modules::access::MembersRead;

/// The user joined a workspace, sent to their streams by each operation that adds a member, such
/// as creating the workspace (WORKSPACES-001).
pub(crate) const MEMBERSHIP_CREATED: UserEvent<MembershipCreated> =
    UserEvent::new("membership.created");
/// A member was removed, sent to the members who may list members (WORKSPACES-005).
pub(super) const MEMBER_REMOVED: WorkspaceEvent<MembersRead, MemberRemoved> =
    WorkspaceEvent::new("member.removed");

/// The payload of `membership.created`: the user joined a workspace.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct MembershipCreated {
    /// The workspace they joined.
    pub(crate) workspace_id: WorkspaceId,
    /// The session whose request added them, so that device can tell it already knows.
    pub(crate) session_id: SessionId,
}

/// The payload of `member.removed`: a member was removed from the workspace.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct MemberRemoved {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The member who was removed.
    pub(super) user_id: UserId,
}
