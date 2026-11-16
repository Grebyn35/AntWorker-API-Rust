//! The audit actions `accounts` records, and the kinds of resource they happen to (ARCH-03, the
//! audit spec's "The action catalog").
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::audit::{Action, Catalogued, Resource};

/// The user renamed themselves (ACCOUNTS-007).
pub(super) const USER_RENAMED: Action =
    Action::new("user.renamed", "The user changed their name.", &[USER]).in_own_log();

/// A user.
pub(super) const USER: Resource = Resource::new("user");

/// The user requested an export of their data (ACCOUNTS-020).
pub(super) const EXPORT_REQUESTED: Action = Action::new(
    "export.requested",
    "The user asked for an export of their data.",
    &[EXPORT],
)
.in_own_log();
/// An export's archive was built, as its job records it in the user's own log (ACCOUNTS-023).
pub(super) const EXPORT_COMPLETED: Action = Action::new(
    "export.completed",
    "The archive of an export of the user's data was built, ready to download.",
    &[EXPORT],
)
.in_own_log();
/// The last attempt of an export's job failed, as it records it (ACCOUNTS-023).
pub(super) const EXPORT_FAILED: Action = Action::new(
    "export.failed",
    "An export of the user's data failed, and holds no archive.",
    &[EXPORT],
)
.in_own_log();
/// The user downloaded their export's archive, or a range of it (ACCOUNTS-026).
pub(super) const EXPORT_DOWNLOADED: Action = Action::new(
    "export.downloaded",
    "The user downloaded the archive of an export of their data, or part of it.",
    &[EXPORT],
)
.in_own_log();

/// An export of a user's data.
pub(super) const EXPORT: Resource = Resource::new("export");

// part: files
/// The user set an avatar (ACCOUNTS-008).
pub(super) const AVATAR_UPLOADED: Action =
    Action::new("avatar.uploaded", "The user set their avatar.", &[AVATAR]).in_own_log();
/// The user removed their avatar (ACCOUNTS-010).
pub(super) const AVATAR_DELETED: Action = Action::new(
    "avatar.deleted",
    "The user removed their avatar.",
    &[AVATAR],
)
.in_own_log();
/// A job copied the picture that a provider gave for the user as their avatar (ACCOUNTS-028).
pub(super) const AVATAR_COPIED: Action = Action::new(
    "avatar.copied",
    "The picture that a sign-in provider gave for the user became their avatar.",
    &[AVATAR],
)
.in_own_log();

/// A user's avatar.
pub(super) const AVATAR: Resource = Resource::new("avatar");
// end part: files

/// Every action `accounts` records, which the action catalog lists (AUDIT-012).
pub(crate) const ACTIONS: &[&dyn Catalogued] = &[
    &USER_RENAMED,
    &EXPORT_REQUESTED,
    &EXPORT_COMPLETED,
    &EXPORT_FAILED,
    &EXPORT_DOWNLOADED,
    // part: files
    &AVATAR_UPLOADED,
    &AVATAR_DELETED,
    &AVATAR_COPIED,
    // end part: files
];
