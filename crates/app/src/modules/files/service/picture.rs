//! Workspaces' pictures, which `workspaces` sets and removes (the files spec's "Workspace
//! pictures", D149): a picture's upload, with the checks of any upload; its row, which
//! `workspaces` records in its own unit of work; and the links of workspaces' pictures.

use bytes::Bytes;
use platform::{db::UnitOfWork, types::WorkspaceId};

use super::{Files, Image, upload_image};
use crate::modules::files::{error::FilesError, repo::picture as repo};

/// The most bytes a workspace's picture holds, fixed, as an avatar's limit is, since every list
/// of a user's workspaces downloads each one's picture, with which the route of `workspaces`
/// reads its body (WORKSPACES-020, D149).
pub(crate) const PICTURE_MAX_BYTES: usize = 1_048_576;

/// The prefix of the keys of workspaces' pictures in the bucket.
pub(in crate::modules::files) const PICTURE_PREFIX: &str = "pictures/";

/// A picture whose bytes are checked and scanned, and whose object is written, which
/// [`record_picture`] makes a workspace's.
pub(crate) struct NewPicture(Image);

/// Finds the kind of a picture's `bytes`, which its route read, among the image kinds, from the
/// bytes alone; has the scanner scan them; writes their object outside any transaction; then has
/// `record` make it the workspace's picture, in a unit of work of its caller's, which calls
/// [`record_picture`] (WORKSPACES-020, DATA-21). An object whose row never commits is left to the
/// deletion of unreferenced objects (FILES-011), and the upload counts once in `files.uploads`,
/// by how it ended (FILES-013).
pub(crate) async fn upload_picture<T, E, R>(
    files: &Files,
    bytes: Bytes,
    record: impl FnOnce(NewPicture) -> R,
) -> Result<T, E>
where
    R: Future<Output = Result<T, E>>,
    E: From<FilesError>,
{
    upload_image(files, PICTURE_PREFIX, bytes, |image| {
        record(NewPicture(image))
    })
    .await
}

/// Makes `picture` the picture of `workspace`, in place of any it had, whose row it deletes, and
/// gives its ID. Its caller has locked the workspace's row in `uow` already, so two changes of
/// one workspace's picture take turns (the files spec's "Workspace pictures").
pub(crate) async fn record_picture(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    picture: NewPicture,
) -> Result<uuid::Uuid, FilesError> {
    let NewPicture(Image { kind, object }) = picture;
    repo::delete(uow, workspace).await?;
    Ok(repo::insert(
        uow,
        workspace,
        kind.media_type(),
        object.size_bytes,
        object.crc32,
        &object.key,
    )
    .await?)
}

/// Deletes the picture of `workspace`, and gives its ID, if it had one. Its object stays until
/// the deletion of unreferenced objects deletes it (FILES-011).
pub(crate) async fn delete_picture(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
) -> Result<Option<uuid::Uuid>, FilesError> {
    Ok(repo::delete(uow, workspace).await?)
}

/// The path of the public download of each picture among the workspaces `workspaces` names, for
/// those that have one, which came from a query filtered by tenant, or are the one a proof names
/// (FILES-027, DATA-16).
pub(crate) async fn picture_urls(
    uow: &mut UnitOfWork,
    workspaces: &[WorkspaceId],
) -> Result<Vec<(WorkspaceId, String)>, FilesError> {
    Ok(repo::ids(uow, workspaces)
        .await?
        .into_iter()
        .map(|(workspace, id)| (workspace, format!("/v1/public/workspace-pictures/{id}")))
        .collect())
}
