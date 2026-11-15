//! The invitation's domain types (D25), and what its routes answer.

use platform::{
    rate_limit::Bucket,
    types::{EmailAddress, Timestamp, UserId, Version, WorkspaceId},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::modules::{access::Role, accounts::DisplayName, workspaces::WorkspaceName};

/// What inviting takes (INVITATIONS-001).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewInvitation {
    /// The invitee's address.
    pub(crate) email: EmailAddress,
    /// The role the invitation offers, which must rank below the inviter's (ACCESS-004).
    pub(crate) role: Role,
}

/// An invitation to a workspace (INVITATIONS-001, INVITATIONS-011).
// `Deserialize` only reads back a stored result that an idempotency key replays (PLATFORM-036).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub(crate) struct Invitation {
    /// The invitation's ID.
    pub(crate) id: uuid::Uuid,
    /// The workspace it offers to join.
    pub(crate) workspace_id: WorkspaceId,
    /// The invitee's address.
    pub(crate) email: EmailAddress,
    /// The role it offers.
    pub(crate) role: Role,
    /// The member who invited, refreshed the invitation or sent it again last, or null once
    /// their account is deleted (ACCOUNTS-018).
    pub(crate) inviter_id: Option<UserId>,
    /// When it was first created.
    pub(crate) created_at: Timestamp,
    /// When it expires.
    pub(crate) expires_at: Timestamp,
}

/// What inviting answered, which an idempotency key keeps: the invitation, with its row's
/// version, and whether inviting created it or refreshed one (INVITATIONS-001, INVITATIONS-003).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Invited {
    /// Whether the invitation is new.
    pub(crate) created: bool,
    /// The invitation, with the version its `ETag` names.
    pub(crate) current: CurrentInvitation,
}

/// An invitation as it now stands, with the version its `ETag` names (INVITATIONS-011,
/// PLATFORM-071).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CurrentInvitation {
    /// The invitation.
    pub(crate) invitation: Invitation,
    /// Its row's version.
    pub(crate) version: Version,
}

/// What changing an invitation's role takes (INVITATIONS-013).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NewRole {
    /// The role the invitation is to offer.
    pub(crate) role: Role,
}

/// One of the caller's own invitations (INVITATIONS-004).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct ReceivedInvitation {
    /// The invitation's ID.
    pub(crate) id: uuid::Uuid,
    /// The workspace it offers to join.
    pub(crate) workspace_id: WorkspaceId,
    /// The workspace's name.
    pub(crate) workspace_name: WorkspaceName,
    /// The name of the member who invited, or null once their account is deleted
    /// (ACCOUNTS-014).
    pub(crate) inviter_name: Option<DisplayName>,
    /// The role it offers.
    pub(crate) role: Role,
    /// When it expires.
    pub(crate) expires_at: Timestamp,
}

/// What accepting an invitation answers: the workspace joined, at its role (INVITATIONS-005).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct Accepted {
    /// The workspace.
    pub(crate) workspace_id: WorkspaceId,
    /// The caller's role in it.
    pub(crate) role: Role,
}

/// An invitation as listing reads it, before the names are added.
#[derive(Clone, Debug)]
pub(super) struct Pending {
    pub(super) id: uuid::Uuid,
    pub(super) workspace_id: WorkspaceId,
    /// `None` once the inviter's account is deleted (ACCOUNTS-014).
    pub(super) inviter_id: Option<UserId>,
    pub(super) role: Role,
    pub(super) created_at: Timestamp,
    pub(super) expires_at: Timestamp,
}

/// The invitation that accepting found, by its ID and the caller's address.
#[derive(Clone, Copy, Debug)]
pub(super) struct Found {
    pub(super) role: Role,
    pub(super) expired: bool,
}

/// How long an invitation lasts, and how many a member may send (INVITATIONS-001,
/// INVITATIONS-009).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Settings {
    /// Its days, from 1 to 90.
    pub(crate) expiry_days: u32,
    /// The per-member limit on inviting, which bounds the emails a member's invitations send
    /// (SEC-13).
    pub(crate) limit: Bucket,
}

/// An invitation of a workspace whose only member is the user, as their export holds it (the
/// accounts spec's "What an export holds").
#[derive(Debug, Serialize)]
pub(super) struct ExportedInvitation {
    pub(super) id: uuid::Uuid,
    pub(super) workspace_id: WorkspaceId,
    pub(super) email: EmailAddress,
    pub(super) role: Role,
    pub(super) expires_at: Timestamp,
    pub(super) created_at: Timestamp,
    pub(super) updated_at: Timestamp,
}
