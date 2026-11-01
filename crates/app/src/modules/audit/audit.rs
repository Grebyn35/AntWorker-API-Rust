//! The audit actions `audit` records itself, and the kinds of resource they happen to (ARCH-03,
//! the audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use super::model::{Action, Resource};

/// Authorization refused a workspace request (AUDIT-004).
pub(super) const ACCESS_DENIED: Action = Action::new("access.denied");

/// A workspace.
pub(super) const WORKSPACE: Resource = Resource::new("workspace");
