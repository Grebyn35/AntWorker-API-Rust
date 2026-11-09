//! The audit actions `accounts` records, and the kinds of resource they happen to (ARCH-03, the
//! audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::audit::{Action, Resource};

/// The user renamed themselves (ACCOUNTS-007).
pub(super) const USER_RENAMED: Action = Action::new("user.renamed");

/// A user.
pub(super) const USER: Resource = Resource::new("user");

/// The user requested an export of their data (ACCOUNTS-020).
pub(super) const DATA_EXPORT_REQUESTED: Action = Action::new("data_export.requested");
/// The user downloaded their export's archive, or a range of it (ACCOUNTS-026).
pub(super) const DATA_EXPORT_DOWNLOADED: Action = Action::new("data_export.downloaded");

/// An export of a user's data.
pub(super) const DATA_EXPORT: Resource = Resource::new("data_export");

// part: files
/// The user set an avatar (ACCOUNTS-008).
pub(super) const AVATAR_UPLOADED: Action = Action::new("avatar.uploaded");
/// The user removed their avatar (ACCOUNTS-010).
pub(super) const AVATAR_DELETED: Action = Action::new("avatar.deleted");

/// A user's avatar.
pub(super) const AVATAR: Resource = Resource::new("avatar");
// end part: files
