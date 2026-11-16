//! The audit actions `invitations` records, and the kind of resource they happen to (ARCH-03,
//! the audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::{
    access::{InvitationsRead, MembersRead},
    audit::{Action, Resource},
};

/// An invitation was created (INVITATIONS-001).
pub(super) const INVITATION_CREATED: Action =
    Action::read_with::<InvitationsRead>("invitation.created");
/// An invitation was refreshed in place (INVITATIONS-003).
pub(super) const INVITATION_UPDATED: Action =
    Action::read_with::<InvitationsRead>("invitation.updated");
/// An invitation's role changed (INVITATIONS-013).
pub(super) const INVITATION_ROLE_CHANGED: Action =
    Action::read_with::<InvitationsRead>("invitation.role_changed");
/// An invitation was sent again (INVITATIONS-015).
pub(super) const INVITATION_RESENT: Action =
    Action::read_with::<InvitationsRead>("invitation.resent");
/// An invitation was cancelled (INVITATIONS-016).
pub(super) const INVITATION_CANCELLED: Action =
    Action::read_with::<InvitationsRead>("invitation.cancelled");
/// An invitation was accepted, and its invitee joined the workspace (INVITATIONS-005).
pub(super) const INVITATION_ACCEPTED: Action =
    Action::read_with::<MembersRead>("invitation.accepted");
/// An invitation was declined by its invitee (INVITATIONS-017).
pub(super) const INVITATION_DECLINED: Action =
    Action::read_with::<InvitationsRead>("invitation.declined");

/// An invitation.
pub(super) const INVITATION: Resource = Resource::new("invitation");
