//! Creating, listing, reading, renaming and deleting a workspace's files (the files
//! spec).

use platform::{
    authz::Authorized,
    db::UnitOfWork,
    errors::RequestError,
    idempotency::{Claimed, IdempotencyKey},
    lists::{List, Page, Search, key},
    types::Version,
};

use super::{
    audit,
    error::FilesError,
    events as published,
    model::{Current, File, NewFile, Rename},
    repo,
};
use crate::modules::{
    access::{FilesCreate, FilesDelete, FilesRead, FilesUpdate},
    audit::{self as log, Entry},
};

/// Creates a file in the caller's workspace, in the unit of work it was authorized in, which
/// claims `key` first, so a retry with the same key and request answers the same file. The
/// file, its audit entry and the members' event commit together (FILES-001,
/// PLATFORM-036).
pub(crate) async fn create(
    mut uow: UnitOfWork,
    authorized: &Authorized<FilesCreate>,
    key: Option<&IdempotencyKey>,
    input: NewFile,
) -> Result<Current, FilesError> {
    if let Claimed::Replay(created) = uow.claim(authorized.principal().user_id(), key).await? {
        return Ok(created);
    }
    let workspace = authorized.workspace_id();
    let created = repo::insert(&mut uow, workspace, &input.name).await?;
    let id = created.file.id;
    let entry = Entry::new(audit::FILE_CREATED, audit::FILE, id);
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    let payload = published::FileChanged {
        workspace_id: workspace,
        file_id: id,
    };
    uow.publish_to_workspace(&published::FILE_CREATED, workspace, &payload)
        .await?;
    uow.commit_with(&created).await?;
    Ok(created)
}

/// A page of the caller's workspace's files, the latest created first, kept to those whose
/// name contains `search` (FILES-003).
pub(crate) async fn list(
    uow: &mut UnitOfWork,
    authorized: &Authorized<FilesRead>,
    search: Option<&Search>,
    page: &Page,
) -> Result<List<File>, FilesError> {
    let rows = repo::page(uow, authorized.workspace_id(), search, page).await?;
    Ok(page.list(rows, |file| key(file.created_at, file.id)))
}

/// The file `id` of the caller's workspace as it now stands (FILES-004).
pub(crate) async fn read(
    uow: &mut UnitOfWork,
    authorized: &Authorized<FilesRead>,
    id: uuid::Uuid,
) -> Result<Current, FilesError> {
    repo::read(uow, authorized.workspace_id(), id)
        .await?
        .ok_or(FilesError::NotFound)
}

/// Renames the file `id` of the caller's workspace, in the unit of work it was authorized
/// in: whatever its version when `versions` is `None`, and otherwise only if its version is one of
/// them, so an empty set fails the precondition. The name, its audit entry and the members' event
/// commit together (FILES-005, PLATFORM-071).
pub(crate) async fn rename(
    mut uow: UnitOfWork,
    authorized: &Authorized<FilesUpdate>,
    id: uuid::Uuid,
    rename: Rename,
    versions: Option<&[Version]>,
) -> Result<Current, FilesError> {
    let workspace = authorized.workspace_id();
    let Some(renamed) = repo::rename(&mut uow, workspace, id, &rename.name, versions).await? else {
        return Err(if repo::exists(&mut uow, workspace, id).await? {
            RequestError::PreconditionFailed.into()
        } else {
            FilesError::NotFound
        });
    };
    let entry = Entry::new(audit::FILE_RENAMED, audit::FILE, id);
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    let payload = published::FileChanged {
        workspace_id: workspace,
        file_id: id,
    };
    uow.publish_to_workspace(&published::FILE_RENAMED, workspace, &payload)
        .await?;
    uow.commit().await?;
    Ok(renamed)
}

/// Deletes the file `id` of the caller's workspace, in the unit of work it was authorized
/// in: the row, its audit entry and the members' event commit together (FILES-006).
pub(crate) async fn delete(
    mut uow: UnitOfWork,
    authorized: &Authorized<FilesDelete>,
    id: uuid::Uuid,
) -> Result<(), FilesError> {
    let workspace = authorized.workspace_id();
    if !repo::delete(&mut uow, workspace, id).await? {
        return Err(FilesError::NotFound);
    }
    let entry = Entry::new(audit::FILE_DELETED, audit::FILE, id);
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    let payload = published::FileChanged {
        workspace_id: workspace,
        file_id: id,
    };
    uow.publish_to_workspace(&published::FILE_DELETED, workspace, &payload)
        .await?;
    Ok(uow.commit().await?)
}
