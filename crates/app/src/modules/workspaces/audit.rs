//! The audit actions `workspaces` records, and the kinds of resource they happen to (ARCH-03,
//! the audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::audit::{Action, Resource};

/// A workspace was created (WORKSPACES-001).
pub(super) const WORKSPACE_CREATED: Action = Action::new("workspace.created");
/// A workspace was renamed (WORKSPACES-008).
pub(super) const WORKSPACE_RENAMED: Action = Action::new("workspace.renamed");
/// A member was removed (WORKSPACES-005).
pub(super) const MEMBER_REMOVED: Action = Action::new("member.removed");
/// A workspace has another owner (WORKSPACES-015).
pub(super) const WORKSPACE_TRANSFERRED: Action = Action::new("workspace.transferred");
/// A workspace was deleted, recorded in the own log of the owner who deleted it
/// (WORKSPACES-017).
pub(super) const WORKSPACE_DELETED: Action = Action::new("workspace.deleted");
/// A member's role changed (WORKSPACES-012).
pub(super) const MEMBER_ROLE_CHANGED: Action = Action::new("member.role_changed");
/// A member left the workspace, themselves or as their account was deleted (WORKSPACES-014,
/// ACCOUNTS-014).
pub(super) const MEMBER_LEFT: Action = Action::new("member.left");

/// A workspace, which an action on one of its members names too, with the member as its target
/// (AUDIT-006).
pub(super) const WORKSPACE: Resource = Resource::new("workspace");

// part: files
/// A workspace's picture was set (WORKSPACES-019).
pub(super) const PICTURE_UPLOADED: Action = Action::new("picture.uploaded");
/// A workspace's picture was removed (WORKSPACES-021).
pub(super) const PICTURE_DELETED: Action = Action::new("picture.deleted");

/// A workspace's picture.
pub(super) const PICTURE: Resource = Resource::new("picture");
// end part: files

// part: workspace_setup
/// The operator's setup of a workspace created its owner's account, recorded in that account's
/// log (WORKSPACES-024).
pub(super) const USER_CREATED: Action = Action::new("user.created");

/// A user, whose account the operator's setup of a workspace creates.
pub(super) const USER: Resource = Resource::new("user");
// end part: workspace_setup
