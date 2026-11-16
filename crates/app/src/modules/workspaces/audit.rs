//! The audit actions `workspaces` records, the kinds of resource they happen to, and their
//! details (ARCH-03, the audit spec's "The action catalog").
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use serde::Serialize;
use utoipa::ToSchema;

// part: superadmins
use crate::modules::audit::Access;
// end part: superadmins
use crate::modules::{
    access::{MembersRead, Role, WorkspaceRead},
    audit::{Action, Catalogued, Resource},
};

/// A workspace was created (the workspaces spec's "Creating a workspace").
pub(super) const WORKSPACE_CREATED: Action<Acted> = Action::new(
    "workspace.created",
    "A workspace was created.",
    &[WORKSPACE],
)
// part: superadmins
.in_own_log()
// end part: superadmins
.in_workspace_log::<WorkspaceRead>();
/// A workspace was renamed (WORKSPACES-008).
pub(super) const WORKSPACE_RENAMED: Action = Action::new(
    "workspace.renamed",
    "A member renamed the workspace.",
    &[WORKSPACE],
)
.in_workspace_log::<WorkspaceRead>();
/// A member was removed (WORKSPACES-005).
pub(super) const MEMBER_REMOVED: Action = Action::new(
    "member.removed",
    "A member removed another from the workspace.",
    &[WORKSPACE],
)
.in_workspace_log::<MembersRead>();
/// A workspace has another owner (WORKSPACES-015).
pub(super) const WORKSPACE_TRANSFERRED: Action = Action::new(
    "workspace.transferred",
    "The workspace's owner made an admin its owner, and became an admin.",
    &[WORKSPACE],
)
.in_workspace_log::<MembersRead>();
/// A workspace was deleted, recorded in the own log of whoever deleted it (WORKSPACES-017).
pub(super) const WORKSPACE_DELETED: Action<Acted> = Action::new(
    "workspace.deleted",
    "The user deleted a workspace, with everything in it.",
    &[WORKSPACE],
)
.in_own_log();
/// A member's role changed (WORKSPACES-012).
pub(super) const MEMBER_ROLE_CHANGED: Action<ChangedRole> = Action::new(
    "member.role_changed",
    "A member changed the role of another.",
    &[WORKSPACE],
)
.in_workspace_log::<MembersRead>();
/// A member left the workspace, themselves or as their account was deleted (WORKSPACES-014,
/// ACCOUNTS-014).
pub(super) const MEMBER_LEFT: Action = Action::new(
    "member.left",
    "A member left the workspace, themselves or as their account was deleted.",
    &[WORKSPACE],
)
.in_workspace_log::<MembersRead>();

/// A workspace, which an action on one of its members names too, with the member as its target
/// (AUDIT-006).
pub(super) const WORKSPACE: Resource = Resource::new("workspace");

// part: files
/// A workspace's picture was set (WORKSPACES-019).
pub(super) const PICTURE_UPLOADED: Action = Action::new(
    "picture.uploaded",
    "A member set the workspace's picture.",
    &[PICTURE],
)
.in_workspace_log::<WorkspaceRead>();
/// A workspace's picture was removed (WORKSPACES-021).
pub(super) const PICTURE_DELETED: Action = Action::new(
    "picture.deleted",
    "A member removed the workspace's picture.",
    &[PICTURE],
)
.in_workspace_log::<WorkspaceRead>();

/// A workspace's picture.
pub(super) const PICTURE: Resource = Resource::new("picture");
// end part: files

// part: workspace_setup
/// The operator's setup of a workspace created its owner's account, recorded in that account's
/// log (WORKSPACES-024).
pub(super) const USER_CREATED: Action = Action::new(
    "user.created",
    "An operator's setup of a workspace created the user's account, as the workspace's owner.",
    &[USER],
)
.in_own_log();

/// A user, whose account the operator's setup of a workspace creates.
pub(super) const USER: Resource = Resource::new("user");
// end part: workspace_setup

/// Every action `workspaces` records, which the action catalog lists (AUDIT-012).
pub(crate) const ACTIONS: &[&dyn Catalogued] = &[
    &WORKSPACE_CREATED,
    &WORKSPACE_RENAMED,
    &MEMBER_REMOVED,
    &WORKSPACE_TRANSFERRED,
    &WORKSPACE_DELETED,
    &MEMBER_ROLE_CHANGED,
    &MEMBER_LEFT,
    // part: files
    &PICTURE_UPLOADED,
    &PICTURE_DELETED,
    // end part: files
    // part: workspace_setup
    &USER_CREATED,
    // end part: workspace_setup
];

/// The details of a change of a role.
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub(crate) struct ChangedRole {
    /// The role before the change.
    pub(crate) from: Role,
    /// The role after it.
    pub(crate) to: Role,
}

/// The details of `workspace.created` and `workspace.deleted`, which say how the caller acted,
/// when they acted other than as a user of the workspace.
#[derive(Clone, Copy, Debug, Default, Serialize, ToSchema)]
pub(super) struct Acted {
    // part: superadmins
    /// `superadmin` for a superadmin's creation or deletion of the workspace (WORKSPACES-030,
    /// WORKSPACES-031).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) access: Option<Access>,
    // end part: superadmins
}
