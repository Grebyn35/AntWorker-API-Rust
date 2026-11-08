//! The events `files` publishes, each with its payload, which the event catalog documents
//! (API-19, REALTIME-008).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its events here (the platform spec's events)"
)]

use platform::{
    events::{UserEvent, WorkspaceEvent},
    types::WorkspaceId,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::modules::access::FilesRead;

/// A file was uploaded, sent to the members who may read files (FILES-001).
pub(super) const FILE_UPLOADED: WorkspaceEvent<FilesRead, FileChanged> =
    WorkspaceEvent::new("file.uploaded");
/// A file was deleted, sent to the members who may read files (FILES-010).
pub(super) const FILE_DELETED: WorkspaceEvent<FilesRead, FileChanged> =
    WorkspaceEvent::new("file.deleted");

/// A file of the user's own was uploaded, sent to their streams (FILES-016).
pub(super) const USER_FILE_UPLOADED: UserEvent<UserFileChanged> =
    UserEvent::new("user_file.uploaded");
/// A file of the user's own was deleted, sent to their streams (FILES-019).
pub(super) const USER_FILE_DELETED: UserEvent<UserFileChanged> =
    UserEvent::new("user_file.deleted");

/// The payload of each file event: which file changed, which a client reads through
/// its route, since a name is personal data (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct FileChanged {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The file.
    pub(super) file_id: uuid::Uuid,
}

/// The payload of each event of a user's own file: which file changed, which a client reads
/// through its route, since a name is personal data (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct UserFileChanged {
    /// The file.
    pub(super) file_id: uuid::Uuid,
}
