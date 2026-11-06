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

use crate::modules::access::{MembersRead, WorkspaceRead};

/// The user joined a workspace, sent to their streams by each operation that adds a member, such
/// as creating the workspace (WORKSPACES-001).
pub(crate) const MEMBERSHIP_CREATED: UserEvent<MembershipCreated> =
    UserEvent::new("membership.created");
/// A member was removed, sent to the members who may list members (WORKSPACES-005).
pub(super) const MEMBER_REMOVED: WorkspaceEvent<MembersRead, MemberRemoved> =
    WorkspaceEvent::new("member.removed");

/// The workspace was renamed, sent to every member, all of whom may read it (WORKSPACES-008).
pub(super) const WORKSPACE_RENAMED: WorkspaceEvent<WorkspaceRead, WorkspaceRenamed> =
    WorkspaceEvent::new("workspace.renamed");

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

/// The payload of `workspace.renamed`: the workspace has a new name, which a client reads through
/// `GET /v1/workspaces/{workspace_id}`, since a name is personal data (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct WorkspaceRenamed {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
}
