//! The audit actions `files` records, and the kind of resource they happen to (ARCH-03, the
//! audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::audit::{Action, Resource};

/// A file was created (FILES-001).
pub(super) const FILE_CREATED: Action = Action::new("file.created");
/// A file was renamed (FILES-005).
pub(super) const FILE_RENAMED: Action = Action::new("file.renamed");
/// A file was deleted (FILES-006).
pub(super) const FILE_DELETED: Action = Action::new("file.deleted");

/// A file.
pub(super) const FILE: Resource = Resource::new("file");
