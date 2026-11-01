//! The invitation's domain types (D25), and what its routes answer.

use platform::{
    rate_limit::Bucket,
    types::{EmailAddress, Timestamp, UserId, WorkspaceId},
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

/// An invitation to a workspace (INVITATIONS-001). `Deserialize` only reads back a stored result
/// that an idempotency key replays (PLATFORM-018).
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
    /// The member who invited, or refreshed the invitation last.
    pub(crate) inviter_id: UserId,
    /// When it was first created.
    pub(crate) created_at: Timestamp,
    /// When it expires.
    pub(crate) expires_at: Timestamp,
}

/// What inviting answered, which an idempotency key keeps: the invitation, and whether inviting
/// created it or refreshed one (INVITATIONS-001, INVITATIONS-003).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Invited {
    /// Whether the invitation is new.
    pub(crate) created: bool,
    /// The invitation.
    pub(crate) invitation: Invitation,
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
    /// The name of the member who invited.
    pub(crate) inviter_name: DisplayName,
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
    pub(super) inviter_id: UserId,
    pub(super) role: Role,
    pub(super) created_at: Timestamp,
    pub(super) expires_at: Timestamp,
}

/// The invitation that accepting found, by its ID and the caller's address.
#[derive(Clone, Copy, Debug)]
pub(super) struct Found {
    pub(super) workspace_id: WorkspaceId,
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
    /// (SEC-12).
    pub(crate) limit: Bucket,
}
