//! What the module's route answers, and what its subcommand takes and reports.

use platform::types::Timestamp;
use serde::Serialize;
use utoipa::ToSchema;

/// Whether the caller is a superadmin (SUPERADMINS-006).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct SuperadminStanding {
    /// Whether the caller is one of the deployment's superadmins.
    pub(crate) superadmin: bool,
}

/// What the subcommand `superadmin` does with the account that holds the address it is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// Makes the account a superadmin (SUPERADMINS-001).
    Add,
    /// Ends the account's being a superadmin (SUPERADMINS-003).
    Remove,
}

/// Why a run of the subcommand `superadmin` changed nothing (SUPERADMINS-002, SUPERADMINS-003).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// What the operator gave isn't an email address.
    NotAnAddress,
    /// No account holds the address.
    NoAccount,
    /// Email is on, and the account that holds the address hasn't proved it.
    AddressUnproved,
    /// The account is a superadmin already.
    AlreadySuperadmin,
    /// The account is no superadmin.
    NotSuperadmin,
}

/// A user's row of `superadmins`, as their export holds it (SUPERADMINS-007).
#[derive(Debug, Serialize)]
pub(super) struct Exported {
    /// When the operator made the user a superadmin.
    pub(super) created_at: Timestamp,
}
