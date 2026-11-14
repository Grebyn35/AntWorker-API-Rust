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

use crate::modules::access::{MembersRead, Role, WorkspaceRead};

/// The user joined a workspace, sent to their streams by each operation in which a request of
/// their own session adds them.
pub(crate) const MEMBERSHIP_CREATED: UserEvent<MembershipCreated> =
    UserEvent::new("membership.created");
/// A member was removed, sent to the members who may list members (WORKSPACES-005).
pub(super) const MEMBER_REMOVED: WorkspaceEvent<MembersRead, MemberRemoved> =
    WorkspaceEvent::new("member.removed");

/// A member left the workspace, themselves or as their account was deleted, sent to the members
/// who may list members (WORKSPACES-014, ACCOUNTS-014).
pub(super) const MEMBER_LEFT: WorkspaceEvent<MembersRead, MemberLeft> =
    WorkspaceEvent::new("member.left");

/// A member's role changed, sent to the members who may list members (WORKSPACES-012).
pub(super) const MEMBER_ROLE_CHANGED: WorkspaceEvent<MembersRead, MemberRoleChanged> =
    WorkspaceEvent::new("member.role_changed");

/// The workspace has another owner, sent to the members who may list members, since it tells two
/// members' roles (WORKSPACES-015).
pub(super) const WORKSPACE_TRANSFERRED: WorkspaceEvent<MembersRead, WorkspaceTransferred> =
    WorkspaceEvent::new("workspace.transferred");

/// What the workspace's members read of it changed, such as its name, sent to every member, all
/// of whom may read it (WORKSPACES-008).
pub(super) const WORKSPACE_UPDATED: WorkspaceEvent<WorkspaceRead, WorkspaceUpdated> =
    WorkspaceEvent::new("workspace.updated");

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

/// The payload of `member.left`: a member left the workspace.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct MemberLeft {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The member who left.
    pub(super) user_id: UserId,
}

/// The payload of `member.role_changed`: a member holds another role.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct MemberRoleChanged {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The member.
    pub(super) user_id: UserId,
    /// The role they hold.
    pub(super) role: Role,
}

/// The payload of `workspace.transferred`: the workspace has another owner, and the owner before
/// is an admin.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct WorkspaceTransferred {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The member who owns it.
    pub(super) owner_id: UserId,
    /// The member who owned it before, an admin.
    pub(super) previous_owner_id: UserId,
}

/// The payload of `workspace.updated`: the workspace changed, such as its name, which a client
/// reads through `GET /v1/workspaces/{workspace_id}`, since a name is personal data (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct WorkspaceUpdated {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
}
