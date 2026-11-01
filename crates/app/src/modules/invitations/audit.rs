//! The audit actions `invitations` records, and the kind of resource they happen to (ARCH-03,
//! the audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::audit::{Action, Resource};

/// An invitation was created (INVITATIONS-001).
pub(super) const INVITATION_CREATED: Action = Action::new("invitation.created");
/// An invitation was refreshed in place (INVITATIONS-003).
pub(super) const INVITATION_UPDATED: Action = Action::new("invitation.updated");
/// An invitation was accepted, and its invitee joined the workspace (INVITATIONS-005).
pub(super) const INVITATION_ACCEPTED: Action = Action::new("invitation.accepted");

/// An invitation.
pub(super) const INVITATION: Resource = Resource::new("invitation");
