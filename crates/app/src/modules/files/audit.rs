//! The audit actions `files` records, and the kind of resource they happen to (ARCH-03, the
//! audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::audit::{Action, Resource};

/// A file was uploaded (FILES-001).
pub(super) const FILE_UPLOADED: Action = Action::new("file.uploaded");
/// A file was deleted (FILES-010).
pub(super) const FILE_DELETED: Action = Action::new("file.deleted");

/// A file of the user's own was uploaded (FILES-016).
pub(super) const USER_FILE_UPLOADED: Action = Action::new("user_file.uploaded");
/// A file of the user's own was deleted (FILES-019).
pub(super) const USER_FILE_DELETED: Action = Action::new("user_file.deleted");

/// A file of a workspace.
pub(super) const FILE: Resource = Resource::new("file");
/// A file of a user's own.
pub(super) const USER_FILE: Resource = Resource::new("user_file");
