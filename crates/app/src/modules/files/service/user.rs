//! A user's own files: uploading, listing, reading, downloading and deleting them, each for the
//! caller that the request's `Principal` proves, and no one else (the files spec).

use platform::{
    authn::Principal,
    db::UnitOfWork,
    idempotency::Claimed,
    lists::{List, Page, Search, key},
    types::Client,
};

use super::{Files, Sent, Serving, USER_PREFIX, Written, upload as checked_upload};
use crate::modules::{
    audit::{self as log, Entry},
    files::{
        audit,
        error::FilesError,
        events as published,
        model::{OfUser, Recorded, UserFile},
        repo::{NewRow, link_expiry, user as repo},
    },
};

/// Stores the file that `sent` names in the caller's own files, as its visibility says, once its
/// kind is checked, the scanner has scanned it and its object is written; then opens the unit of
/// work, on `client`'s request, which claims the sent key, so a retry with the same key and
/// request answers the same file, linked anew. The row, its audit entry in the caller's own log
/// and their event commit together (FILES-016, FILES-020, FILES-021, PLATFORM-036).
pub(in crate::modules::files) async fn upload(
    files: &Files,
    principal: &Principal,
    client: &Client,
    sent: Sent<'_>,
) -> Result<UserFile, FilesError> {
    let Sent {
        name,
        visibility,
        bytes,
        key,
    } = sent;
    checked_upload(
        files,
        USER_PREFIX,
        name,
        bytes,
        async move |written: Written| {
            let mut uow = files.database.begin(client).await?;
            let until = link_expiry(&mut uow).await?;
            let user = principal.user_id();
            if let Claimed::Replay(file) = uow.claim::<Recorded<OfUser>>(user, key).await? {
                let link = files.user_link(&file, principal.session_id(), until);
                return Ok(UserFile::linked(file, link));
            }
            let file = repo::insert(
                &mut uow,
                user,
                &NewRow {
                    name: &written.name,
                    media_type: written.kind.media_type(),
                    size_bytes: written.size_bytes,
                    visibility,
                    object_key: &written.object_key,
                },
            )
            .await?;
            let entry = Entry::new(audit::USER_FILE_UPLOADED, audit::USER_FILE, file.id);
            log::record(&mut uow, principal.audit_scope(), entry).await?;
            let payload = published::UserFileChanged { file_id: file.id };
            uow.publish_to_user(&published::USER_FILE_UPLOADED, user, &payload)
                .await?;
            uow.commit_with(&file).await?;
            let link = files.user_link(&file, principal.session_id(), until);
            Ok(UserFile::linked(file, link))
        },
    )
    .await
}

/// A page of the caller's own files, the latest uploaded first, kept to those whose name contains
/// `search`, each with its links for the caller's session (FILES-017, FILES-021).
pub(in crate::modules::files) async fn list(
    files: &Files,
    uow: &mut UnitOfWork,
    principal: &Principal,
    search: Option<&Search>,
    page: &Page,
) -> Result<List<UserFile>, FilesError> {
    let rows = repo::page(uow, principal.user_id(), search, page).await?;
    let until = link_expiry(uow).await?;
    let list = page.list(rows, |file| key(file.created_at, file.id));
    Ok(List {
        items: list
            .items
            .into_iter()
            .map(|file| {
                let link = files.user_link(&file, principal.session_id(), until);
                UserFile::linked(file, link)
            })
            .collect(),
        next_cursor: list.next_cursor,
    })
}

/// The file `id` of the caller's own, with its links for the caller's session (FILES-018,
/// FILES-021).
pub(in crate::modules::files) async fn read(
    files: &Files,
    uow: &mut UnitOfWork,
    principal: &Principal,
    id: uuid::Uuid,
) -> Result<UserFile, FilesError> {
    let stored = repo::read(uow, principal.user_id(), id)
        .await?
        .ok_or(FilesError::NotFound)?;
    let until = link_expiry(uow).await?;
    let link = files.user_link(&stored.file, principal.session_id(), until);
    Ok(UserFile::linked(stored.file, link))
}

/// The file `id` of the caller's own, as its download serves it. The unit of work ends once the
/// row is read, before the bucket is asked (FILES-009, DATA-21).
pub(in crate::modules::files) async fn download(
    mut uow: UnitOfWork,
    principal: &Principal,
    id: uuid::Uuid,
) -> Result<Serving, FilesError> {
    let stored = repo::read(&mut uow, principal.user_id(), id).await?;
    drop(uow);
    let stored = stored.ok_or(FilesError::NotFound)?;
    let file = stored.file;
    Ok(Serving {
        id: file.id,
        media_type: file.media_type,
        size_bytes: file.size_bytes,
        object_key: stored.object_key,
        name: Some(file.name.as_str().to_owned()),
    })
}

/// Deletes the file `id` of the caller's own: the row, its audit entry in their own log and their
/// event commit together. Its object stays until the deletion of unreferenced objects deletes it
/// (FILES-019, FILES-011).
pub(in crate::modules::files) async fn delete(
    mut uow: UnitOfWork,
    principal: &Principal,
    id: uuid::Uuid,
) -> Result<(), FilesError> {
    let user = principal.user_id();
    if !repo::delete(&mut uow, user, id).await? {
        return Err(FilesError::NotFound);
    }
    let entry = Entry::new(audit::USER_FILE_DELETED, audit::USER_FILE, id);
    log::record(&mut uow, principal.audit_scope(), entry).await?;
    let payload = published::UserFileChanged { file_id: id };
    uow.publish_to_user(&published::USER_FILE_DELETED, user, &payload)
        .await?;
    Ok(uow.commit().await?)
}
