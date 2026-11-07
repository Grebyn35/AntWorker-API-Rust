//! The events `files` publishes, each with its payload, which the event catalog documents
//! (API-19, REALTIME-008).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its events here (the platform spec's events)"
)]

use platform::{events::WorkspaceEvent, types::WorkspaceId};
use serde::Serialize;
use utoipa::ToSchema;

use crate::modules::access::FilesRead;

/// A file was created, sent to the members who may read files (FILES-001).
pub(super) const FILE_CREATED: WorkspaceEvent<FilesRead, FileChanged> =
    WorkspaceEvent::new("file.created");
/// A file was renamed, sent to the members who may read files (FILES-005).
pub(super) const FILE_RENAMED: WorkspaceEvent<FilesRead, FileChanged> =
    WorkspaceEvent::new("file.renamed");
/// A file was deleted, sent to the members who may read files (FILES-006).
pub(super) const FILE_DELETED: WorkspaceEvent<FilesRead, FileChanged> =
    WorkspaceEvent::new("file.deleted");

/// The payload of each file event: which file changed, which a client reads through
/// its route, since a name is personal data (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct FileChanged {
    /// The workspace.
    pub(super) workspace_id: WorkspaceId,
    /// The file.
    pub(super) file_id: uuid::Uuid,
}
