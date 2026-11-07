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
/// A file whose bytes neither the bucket nor its copy held was removed (FILES-015).
pub(super) const FILE_LOST: Action = Action::new("file.lost");

/// A file.
pub(super) const FILE: Resource = Resource::new("file");
