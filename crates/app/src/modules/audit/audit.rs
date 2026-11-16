//! The audit actions `audit` records itself, the kinds of resource they happen to, and their
//! details (ARCH-03, the audit spec's "The action catalog").
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use serde::Serialize;
use utoipa::ToSchema;

// part: superadmins
use super::model::Access;
// end part: superadmins
use super::model::{Action, Catalogued, Resource};

/// Authorization refused a request (AUDIT-004).
pub(super) const ACCESS_DENIED: Action<Denial> = Action::new(
    "access.denied",
    "A request was refused, since the caller may not do what it asks.",
    &[
        // The workspace whose route refused the request.
        WORKSPACE,
        // part: superadmins
        // The deployment, whose superadmin's route refused it.
        DEPLOYMENT,
        // end part: superadmins
    ],
)
.in_own_log()
.in_workspace_log_for_every_reader();

/// Every action `audit` records, which the action catalog lists (AUDIT-012).
pub(crate) const ACTIONS: &[&dyn Catalogued] = &[&ACCESS_DENIED];

/// A workspace.
pub(super) const WORKSPACE: Resource = Resource::new("workspace");

// part: superadmins
/// The deployment, which a superadmin's routes act on, and which has no ID (SUPERADMINS-004).
pub(super) const DEPLOYMENT: Resource = Resource::new("deployment");
// end part: superadmins

/// The details of `access.denied`: what the refused request needed.
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub(super) struct Denial {
    /// The permission that the route asks of a member of the workspace, which the caller's role
    /// doesn't hold, or which a non-member holds none of.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) permission: Option<&'static str>,
    // part: superadmins
    /// `superadmin` for a superadmin's route, which the caller, who is no superadmin, called.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) access: Option<Access>,
    // end part: superadmins
}
