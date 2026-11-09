//! The events `accounts` publishes, each with its payload, which the event catalog documents
//! (API-19, REALTIME-008).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its events here (the platform spec's events)"
)]

use platform::{
    events::{UserEvent, WorkspaceEvent},
    types::{UserId, WorkspaceId},
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::modules::access::MembersRead;

/// The user's record changed, sent to their own streams (ACCOUNTS-007).
pub(super) const USER_UPDATED: UserEvent<UserUpdated> = UserEvent::new("user.updated");

/// A member's record changed, such as their name, sent in each workspace they belong to, to the
/// members who may list members (ACCOUNTS-007).
pub(super) const MEMBER_UPDATED: WorkspaceEvent<MembersRead, MemberUpdated> =
    WorkspaceEvent::new("member.updated");

/// The user's export is ready to download, sent to their own streams (ACCOUNTS-023).
pub(super) const EXPORT_COMPLETED: UserEvent<ExportChanged> = UserEvent::new("export.completed");

/// The last attempt of the job of the user's export failed, sent to their own streams
/// (ACCOUNTS-023).
pub(super) const EXPORT_FAILED: UserEvent<ExportChanged> = UserEvent::new("export.failed");

/// The payload of `user.updated`: the user's record changed, which a client reads through
/// `GET /v1/users/me`, since a name is personal data (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct UserUpdated {
    /// The user.
    pub(super) user_id: UserId,
}

/// The payload of `export.completed` and `export.failed`: the export, which a client reads
/// through `GET /v1/users/me/exports/{export_id}` (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ExportChanged {
    /// The export.
    pub(super) export_id: uuid::Uuid,
}

/// The payload of `member.updated`: a member's record changed, which a client reads through the
/// workspace's members, since a name is personal data (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct MemberUpdated {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The member.
    pub(super) user_id: UserId,
}
