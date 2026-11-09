//! A workspace's files: uploading, listing, reading, downloading and deleting them, each under
//! the proof that the caller is a member with the route's permission (the files spec).

use platform::{
    authz::{Authorize, Authorized},
    db::UnitOfWork,
    idempotency::Claimed,
    lists::{List, Page, Search, key},
    ranges::Wanted,
};

use super::{
    Downloaded, Files, Sent, Serving, WORKSPACE_PREFIX, Written, link_expiry, serve,
    upload as checked_upload,
};
use crate::modules::{
    access::{FilesDelete, FilesRead, FilesUpload},
    audit::{self as log, Entry},
    files::{
        audit,
        error::FilesError,
        events as published,
        model::{File, InWorkspace, Recorded},
        repo::{self, NewRow},
    },
};

/// Stores the file that `sent` names in the caller's workspace, as its visibility says, once its
/// kind is checked, the scanner has scanned it and its object is written; then opens the unit of
/// work with `authorize`, which authorizes the caller again and claims the sent key, so a retry
/// with the same key and request answers the same file, linked anew. The row, its audit entry and
/// the members' event commit together (FILES-001, FILES-020, FILES-021, PLATFORM-036).
pub(in crate::modules::files) async fn upload(
    files: &Files,
    authorize: Authorize<FilesUpload>,
    sent: Sent<'_>,
) -> Result<File, FilesError> {
    let Sent {
        name,
        visibility,
        bytes,
        key,
    } = sent;
    checked_upload(
        files,
        WORKSPACE_PREFIX,
        name,
        bytes,
        async move |written: Written| {
            let (mut uow, authorized) = authorize.open().await?;
            let principal = authorized.principal().clone();
            let user = principal.user_id();
            // The claim is the unit of work's first statement after the authorization that opened
            // it (the platform spec's Idempotency).
            let claimed = uow.claim::<Recorded<InWorkspace>>(user, key).await?;
            let until = link_expiry(&mut uow).await?;
            if let Claimed::Replay(file) = claimed {
                let link = files.workspace_link(&file, principal.session_id(), until);
                return Ok(File::linked(file, link));
            }
            let workspace = authorized.workspace_id();
            let file = repo::insert(
                &mut uow,
                workspace,
                user,
                &NewRow {
                    name: &written.name,
                    media_type: written.kind.media_type(),
                    size_bytes: written.object.size_bytes,
                    crc32: written.object.crc32,
                    visibility,
                    object_key: &written.object.key,
                },
            )
            .await?;
            let entry = Entry::new(audit::FILE_UPLOADED, audit::FILE, file.id);
            log::record(&mut uow, authorized.audit_scope(), entry).await?;
            let payload = published::FileChanged {
                workspace_id: workspace,
                file_id: file.id,
            };
            uow.publish_to_workspace(&published::FILE_UPLOADED, workspace, &payload)
                .await?;
            uow.commit_with(&file).await?;
            let link = files.workspace_link(&file, principal.session_id(), until);
            Ok(File::linked(file, link))
        },
    )
    .await
}

/// A page of the caller's workspace's files, the latest uploaded first, kept to those whose name
/// contains `search`, each with its links for the caller's session (FILES-007, FILES-021).
pub(in crate::modules::files) async fn list(
    files: &Files,
    uow: &mut UnitOfWork,
    authorized: &Authorized<FilesRead>,
    search: Option<&Search>,
    page: &Page,
) -> Result<List<File>, FilesError> {
    let rows = repo::page(uow, authorized.workspace_id(), search, page).await?;
    let until = link_expiry(uow).await?;
    let session = authorized.principal().session_id();
    let list = page.list(rows, |file| key(file.created_at, file.id));
    Ok(List {
        items: list
            .items
            .into_iter()
            .map(|file| {
                let link = files.workspace_link(&file, session, until);
                File::linked(file, link)
            })
            .collect(),
        next_cursor: list.next_cursor,
    })
}

/// The file `id` of the caller's workspace, with its links for the caller's session (FILES-008,
/// FILES-021).
pub(in crate::modules::files) async fn read(
    files: &Files,
    uow: &mut UnitOfWork,
    authorized: &Authorized<FilesRead>,
    id: uuid::Uuid,
) -> Result<File, FilesError> {
    let stored = repo::read(uow, authorized.workspace_id(), id)
        .await?
        .ok_or(FilesError::NotFound)?;
    let until = link_expiry(uow).await?;
    let link = files.workspace_link(&stored.file, authorized.principal().session_id(), until);
    Ok(File::linked(stored.file, link))
}

/// What a download of the file `id` of the caller's workspace serves, as `wanted` asks for it.
/// The unit of work ends once the row is read, before the bucket is asked, so no transaction
/// waits on it (FILES-009, DATA-21).
pub(in crate::modules::files) async fn download(
    files: &Files,
    mut uow: UnitOfWork,
    authorized: &Authorized<FilesRead>,
    id: uuid::Uuid,
    wanted: &Wanted,
) -> Result<Downloaded, FilesError> {
    let stored = repo::read(&mut uow, authorized.workspace_id(), id).await?;
    drop(uow);
    let stored = stored.ok_or(FilesError::NotFound)?;
    let file = stored.file;
    let serving = Serving {
        id: file.id,
        media_type: file.media_type,
        size_bytes: file.size_bytes,
        object_key: stored.object_key,
        name: file.name.as_str().to_owned(),
        public: false,
    };
    serve(files, serving, wanted).await
}

/// Deletes the file `id` of the caller's workspace, in the unit of work it was authorized in: the
/// row, its audit entry and the members' event commit together. Its object stays until the
/// deletion of unreferenced objects deletes it (FILES-010, FILES-011).
pub(in crate::modules::files) async fn delete(
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
