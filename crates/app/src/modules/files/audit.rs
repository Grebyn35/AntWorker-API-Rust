//! The audit actions `files` records, and the kind of resource they happen to (ARCH-03, the
//! audit spec's "The action catalog").
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::{
    access::FilesRead,
    audit::{Action, Catalogued, Resource},
};

/// A file was uploaded (FILES-001).
pub(super) const FILE_UPLOADED: Action = Action::new(
    "file.uploaded",
    "A member uploaded a file to the workspace.",
    &[FILE],
)
.in_workspace_log::<FilesRead>();
/// A file was deleted (FILES-010).
pub(super) const FILE_DELETED: Action = Action::new(
    "file.deleted",
    "A member deleted a file of the workspace.",
    &[FILE],
)
.in_workspace_log::<FilesRead>();

/// A file of the user's own was uploaded (FILES-016).
pub(super) const USER_FILE_UPLOADED: Action = Action::new(
    "user_file.uploaded",
    "The user uploaded a file of their own.",
    &[USER_FILE],
)
.in_own_log();
/// A file of the user's own was deleted (FILES-019).
pub(super) const USER_FILE_DELETED: Action = Action::new(
    "user_file.deleted",
    "The user deleted a file of their own.",
    &[USER_FILE],
)
.in_own_log();

/// A file of a workspace.
pub(super) const FILE: Resource = Resource::new("file");
/// A file of a user's own.
pub(super) const USER_FILE: Resource = Resource::new("user_file");

/// Every action `files` records, which the action catalog lists (AUDIT-012).
pub(crate) const ACTIONS: &[&dyn Catalogued] = &[
    &FILE_UPLOADED,
    &FILE_DELETED,
    &USER_FILE_UPLOADED,
    &USER_FILE_DELETED,
];
