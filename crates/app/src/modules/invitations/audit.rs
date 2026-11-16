//! The audit actions `invitations` records, the kind of resource they happen to, and their
//! details (ARCH-03, the audit spec's "The action catalog").
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use serde::Serialize;
use utoipa::ToSchema;

use crate::modules::{
    access::{InvitationsRead, MembersRead, Role},
    audit::{Action, Catalogued, Resource},
    workspaces::ChangedRole,
};

/// An invitation was created (INVITATIONS-001).
pub(super) const INVITATION_CREATED: Action<OfferedRole> = Action::new(
    "invitation.created",
    "A member invited an address to join the workspace.",
    &[INVITATION],
)
.in_workspace_log::<InvitationsRead>();
/// An invitation was refreshed in place (INVITATIONS-003).
pub(super) const INVITATION_UPDATED: Action<OfferedRole> = Action::new(
    "invitation.updated",
    "A member invited again an address that an invitation of the workspace awaited, which \
     renewed that invitation.",
    &[INVITATION],
)
.in_workspace_log::<InvitationsRead>();
/// An invitation's role changed (INVITATIONS-013).
pub(super) const INVITATION_ROLE_CHANGED: Action<ChangedRole> = Action::new(
    "invitation.role_changed",
    "A member changed the role that an invitation offers.",
    &[INVITATION],
)
.in_workspace_log::<InvitationsRead>();
/// An invitation was sent again (INVITATIONS-015).
pub(super) const INVITATION_RESENT: Action = Action::new(
    "invitation.resent",
    "A member sent an invitation's email again.",
    &[INVITATION],
)
.in_workspace_log::<InvitationsRead>();
/// An invitation was cancelled (INVITATIONS-016).
pub(super) const INVITATION_CANCELLED: Action = Action::new(
    "invitation.cancelled",
    "A member cancelled an invitation.",
    &[INVITATION],
)
.in_workspace_log::<InvitationsRead>();
/// An invitation was accepted, and its invitee joined the workspace (INVITATIONS-005).
pub(super) const INVITATION_ACCEPTED: Action = Action::new(
    "invitation.accepted",
    "The invitee accepted an invitation, and joined the workspace.",
    &[INVITATION],
)
.in_workspace_log::<MembersRead>();
/// An invitation was declined by its invitee (INVITATIONS-017).
pub(super) const INVITATION_DECLINED: Action = Action::new(
    "invitation.declined",
    "The invitee declined an invitation.",
    &[INVITATION],
)
.in_workspace_log::<InvitationsRead>();

/// An invitation.
pub(super) const INVITATION: Resource = Resource::new("invitation");

/// Every action `invitations` records, which the action catalog lists (AUDIT-012).
pub(crate) const ACTIONS: &[&dyn Catalogued] = &[
    &INVITATION_CREATED,
    &INVITATION_UPDATED,
    &INVITATION_ROLE_CHANGED,
    &INVITATION_RESENT,
    &INVITATION_CANCELLED,
    &INVITATION_ACCEPTED,
    &INVITATION_DECLINED,
];

/// The details of `invitation.created` and `invitation.updated`: the role that the invitation
/// offers.
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub(super) struct OfferedRole {
    /// The role that the invitee would hold.
    pub(super) role: Role,
}
