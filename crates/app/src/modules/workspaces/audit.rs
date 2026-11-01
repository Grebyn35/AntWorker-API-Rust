//! The audit actions `workspaces` records, and the kinds of resource they happen to (ARCH-03,
//! the audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::audit::{Action, Resource};

/// A workspace was created (WORKSPACES-001).
pub(super) const WORKSPACE_CREATED: Action = Action::new("workspace.created");
/// A member was removed (WORKSPACES-005).
pub(super) const MEMBER_REMOVED: Action = Action::new("member.removed");

/// A workspace.
pub(super) const WORKSPACE: Resource = Resource::new("workspace");
/// A member of a workspace, by their user ID.
pub(super) const MEMBER: Resource = Resource::new("member");
