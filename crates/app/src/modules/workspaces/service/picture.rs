//! Setting and removing a workspace's picture, whose image `files` keeps, and the pictures' links
//! in the workspaces that `workspaces` answers (the workspaces spec's "The picture", D149).

use std::collections::HashMap;

use bytes::Bytes;
use platform::{
    authz::{Authorize, Authorized},
    blocking::Workers,
    db::UnitOfWork,
    errors::RequestError,
    types::{Version, WorkspaceId},
};

use super::{current, publish_updated};
use crate::modules::{
    access::WorkspaceUpdate,
    audit::{self as log, Entry},
    files::{self, Files, NewPicture},
    workspaces::{audit, error::WorkspacesError, model::Current, repo},
};

/// What the picture's routes take: `files`, which keeps the images, and the turns of the
/// instance's CPU-heavy work, which `Upload::read` takes (PLATFORM-074).
#[derive(Clone)]
pub(crate) struct Pictures {
    files: Files,
    pub(in crate::modules::workspaces) workers: Workers,
}

impl Pictures {
    /// The pictures' service, keeping images in `files`, with `workers`.
    pub(crate) fn new(files: Files, workers: Workers) -> Self {
        Self { files, workers }
    }
}

/// Makes the image whose `bytes` the request sent the picture of the caller's workspace, once
/// `files` has checked, scanned and written it, when the workspace's version is one of
/// `versions`, or whatever it is when there are none, then answers the workspace as it then is.
/// The unit of work opens with `authorize`, which authorizes the caller again: the workspace's
/// new version, which locks its row, the picture's row, the audit entry in the workspace's log
/// and the members' event commit together (WORKSPACES-019, WORKSPACES-020, PLATFORM-071).
pub(in crate::modules::workspaces) async fn set(
    pictures: &Pictures,
    authorize: Authorize<WorkspaceUpdate>,
    bytes: Bytes,
    versions: Option<&[Version]>,
) -> Result<Current, WorkspacesError> {
    files::upload_picture(&pictures.files, bytes, async |picture: NewPicture| {
        let (mut uow, authorized) = authorize.open().await?;
        let workspace = authorized.workspace_id();
        if !repo::picture::touch(&mut uow, workspace, versions).await? {
            return Err(RequestError::PreconditionFailed.into());
        }
        let id = files::record_picture(&mut uow, workspace, picture).await?;
        let entry = Entry::new(audit::PICTURE_UPLOADED, audit::PICTURE, id);
        log::record(&mut uow, authorized.audit_scope(), entry).await?;
        publish_updated(&mut uow, workspace).await?;
        let current = current(&mut uow, workspace, authorized.role()).await?;
        uow.commit().await?;
        Ok(current)
    })
    .await
}

/// Removes the picture of the caller's workspace, when it has one, and its version is one of
/// `versions`, or whatever it is when there are none, in the unit of work it was authorized in:
/// the workspace's new version, the picture row's deletion, the audit entry in the workspace's
/// log and the members' event commit together. Without one, nothing changes (WORKSPACES-021,
/// PLATFORM-071).
pub(in crate::modules::workspaces) async fn remove(
    mut uow: UnitOfWork,
    authorized: &Authorized<WorkspaceUpdate>,
    versions: Option<&[Version]>,
) -> Result<(), WorkspacesError> {
    let workspace = authorized.workspace_id();
    if !repo::picture::touch(&mut uow, workspace, versions).await? {
        return Err(RequestError::PreconditionFailed.into());
    }
    // Without a picture, the unit of work rolls back as it is dropped, the version's move with
    // it.
    let Some(id) = files::delete_picture(&mut uow, workspace).await? else {
        return Ok(());
    };
    let entry = Entry::new(audit::PICTURE_DELETED, audit::PICTURE, id);
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    publish_updated(&mut uow, workspace).await?;
    Ok(uow.commit().await?)
}

/// The link of the picture of the workspace `id`, the one a proof names, if it has one
/// (WORKSPACES-022).
pub(super) async fn url(
    uow: &mut UnitOfWork,
    id: WorkspaceId,
) -> Result<Option<String>, WorkspacesError> {
    Ok(files::picture_urls(uow, &[id])
        .await?
        .into_iter()
        .next()
        .map(|(_, url)| url))
}

/// The link of the picture of each workspace among `ids` that has one, which came from a query
/// filtered by tenant, such as the caller's own memberships (WORKSPACES-022, DATA-16).
pub(super) async fn urls(
    uow: &mut UnitOfWork,
    ids: &[WorkspaceId],
) -> Result<HashMap<WorkspaceId, String>, WorkspacesError> {
    Ok(files::picture_urls(uow, ids).await?.into_iter().collect())
}
