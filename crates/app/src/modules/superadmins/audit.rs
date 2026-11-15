//! The audit actions `superadmins` records, and the kind of resource they happen to (ARCH-03,
//! the audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::audit::{Action, Resource};

/// An operator made the account a superadmin, recorded in the account's own log
/// (SUPERADMINS-001).
pub(super) const SUPERADMIN_ADDED: Action = Action::new("superadmin.added");
/// An operator ended the account's being a superadmin, recorded in the account's own log
/// (SUPERADMINS-003).
pub(super) const SUPERADMIN_REMOVED: Action = Action::new("superadmin.removed");

/// A user, whom an operator makes a superadmin, or whose being one an operator ends.
pub(super) const USER: Resource = Resource::new("user");
