//! Uploading, listing, reading, downloading and deleting a workspace's files (the files spec).

use bytes::Bytes;
use platform::{
    authz::{Authorize, Authorized},
    blocking::Workers,
    db::{Database, UnitOfWork},
    errors::Internal,
    idempotency::{Claimed, IdempotencyKey},
    lists::{List, Page, Search, key},
    scanner::{ScanError, Scanner, Verdict},
    storage::{Object, ObjectKey, Storage, StorageError},
};

use super::{
    audit,
    error::FilesError,
    events as published,
    metrics::{self, Upload},
    model::{File, FileName, Kind, Settings, Stored},
    repo,
};
use crate::modules::{
    access::{FilesDelete, FilesRead, FilesUpload},
    audit::{self as log, Entry},
};

/// The prefix of the keys of workspaces' files in the bucket.
pub(super) const WORKSPACE_PREFIX: &str = "workspaces/";

/// What the routes and jobs of `files` share: the database, the bucket, the malware scanner,
/// the turns of the instance's CPU-heavy work, which reading and checking an upload's bytes take,
/// and the module's settings.
#[derive(Clone)]
pub(crate) struct Files {
    pub(super) database: Database,
    pub(super) storage: Storage,
    pub(super) scanner: Scanner,
    pub(super) workers: Workers,
    pub(super) settings: Settings,
}

impl Files {
    /// The module's state, on `database`, `storage`, `scanner` and `workers`.
    pub(crate) fn new(
        database: Database,
        storage: Storage,
        scanner: Scanner,
        workers: Workers,
        settings: Settings,
    ) -> Self {
        Self {
            database,
            storage,
            scanner,
            workers,
            settings,
        }
    }
}

/// Stores the file `name`, with `bytes`, in the caller's workspace: checks its kind, has the
/// scanner scan its bytes, writes its object outside any transaction, then opens the unit of work
/// with `authorize`, which authorizes the caller again and claims `key`, so a retry with the same
/// key and request answers the same file. The row, its audit entry and the members' event commit
/// together (FILES-001, FILES-003, FILES-004, FILES-005, DATA-21, PLATFORM-036).
pub(crate) async fn upload(
    files: &Files,
    authorize: Authorize<FilesUpload>,
    name: FileName,
    bytes: Bytes,
    key: Option<&IdempotencyKey>,
) -> Result<File, FilesError> {
    if bytes.is_empty() {
        return Err(FilesError::Empty);
    }
    let counted = metrics::Counted::start();
    // A text's check reads every byte, so the check is CPU-heavy work (CODE-05).
    let checked = bytes.clone();
    let kind = files
        .workers
        .run("upload kind check", move || {
            Kind::of_name(name.as_str())
                .filter(|kind| kind.holds(&checked))
                .map(|kind| (kind, name))
        })
        .await
        .map_err(Internal::new)?;
    let Some((kind, name)) = kind else {
        counted.end(Upload::UnsupportedType);
        return Err(FilesError::UnsupportedType);
    };
    match files.scanner.scan(&bytes).await {
        Ok(Verdict::Clean) => {}
        Ok(Verdict::Found(_)) => {
            counted.end(Upload::MalwareDetected);
            return Err(FilesError::MalwareDetected);
        }
        Err(error) => {
            counted.end(Upload::Failed);
            return Err(match error {
                ScanError::Unavailable(cause) => FilesError::ScannerUnavailable(Some(cause)),
                ScanError::Failed(cause) => FilesError::Internal(cause),
            });
        }
    }
    let stored = store(files, authorize, name, kind, bytes, key).await;
    counted.end(if stored.is_ok() {
        Upload::Stored
    } else {
        Upload::Failed
    });
    stored
}

/// Writes the object of a file whose kind is checked, then records the file (FILES-001). An
/// object whose row never commits is left to the deletion of unreferenced objects, never deleted
/// here: when a commit's answer is lost, the row may exist (FILES-011).
async fn store(
    files: &Files,
    authorize: Authorize<FilesUpload>,
    name: FileName,
    kind: Kind,
    bytes: Bytes,
    key: Option<&IdempotencyKey>,
) -> Result<File, FilesError> {
    let size_bytes = u64::try_from(bytes.len()).map_err(Internal::new)?;
    let object_key = ObjectKey::random(WORKSPACE_PREFIX)?;
    files
        .storage
        .put(&object_key, bytes, kind.media_type())
        .await
        .map_err(unavailable)?;
    let (mut uow, authorized) = authorize.open().await?;
    let user = authorized.principal().user_id();
    if let Claimed::Replay(file) = uow.claim(user, key).await? {
        return Ok(file);
    }
    let workspace = authorized.workspace_id();
    let file = repo::insert(
        &mut uow,
        workspace,
        user,
        &repo::NewRow {
            name: &name,
            media_type: kind.media_type(),
            size_bytes,
            object_key: &object_key,
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
    Ok(file)
}

/// A page of the caller's workspace's files, the latest uploaded first, kept to those whose name
/// contains `search` (FILES-007).
pub(crate) async fn list(
    uow: &mut UnitOfWork,
    authorized: &Authorized<FilesRead>,
    search: Option<&Search>,
    page: &Page,
) -> Result<List<File>, FilesError> {
    let rows = repo::page(uow, authorized.workspace_id(), search, page).await?;
    Ok(page.list(rows, |file| key(file.created_at, file.id)))
}

/// The file `id` of the caller's workspace (FILES-008).
pub(crate) async fn read(
    uow: &mut UnitOfWork,
    authorized: &Authorized<FilesRead>,
    id: uuid::Uuid,
) -> Result<File, FilesError> {
    let stored = repo::read(uow, authorized.workspace_id(), id)
        .await?
        .ok_or(FilesError::NotFound)?;
    Ok(stored.file)
}

/// The file `id` of the caller's workspace, with its bytes as the bucket streams them. The unit of
/// work ends once the row is read, before the bucket is asked, so no transaction waits on it
/// (FILES-009, DATA-21). A row whose object the bucket doesn't hold is an internal error, whose
/// cause names the file, since only a fault of the bucket's leaves one (FILES-009, ERR-10).
pub(crate) async fn download(
    files: &Files,
    mut uow: UnitOfWork,
    authorized: &Authorized<FilesRead>,
    id: uuid::Uuid,
) -> Result<(Stored, Object), FilesError> {
    let stored = repo::read(&mut uow, authorized.workspace_id(), id).await?;
    drop(uow);
    let stored = stored.ok_or(FilesError::NotFound)?;
    match files
        .storage
        .get(&ObjectKey::from_stored(&stored.object_key))
        .await
    {
        Ok(object) if object.size == stored.file.size_bytes => Ok((stored, object)),
        // An object of another size isn't the file's bytes, as one cut short isn't.
        Ok(object) => Err(FilesError::Internal(Internal::new(format!(
            "the row of the file {} names an object of {} bytes, which the bucket holds with {}",
            stored.file.id, stored.file.size_bytes, object.size
        )))),
        Err(StorageError::NotFound) => Err(FilesError::Internal(Internal::new(format!(
            "the row of the file {} names an object that the bucket doesn't hold",
            stored.file.id
        )))),
        Err(error) => Err(unavailable(error)),
    }
}

/// Deletes the file `id` of the caller's workspace, in the unit of work it was authorized in: the
/// row, its audit entry and the members' event commit together. Its object stays until the
/// deletion of unreferenced objects deletes it (FILES-010, FILES-011).
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

/// The module's failure for a failure of the bucket: unavailable when it didn't answer
/// (FILES-012), and internal otherwise, an object that isn't there included, which no caller of
/// this function expects (ERR-04).
fn unavailable(error: StorageError) -> FilesError {
    match error {
        StorageError::Unavailable(error) => FilesError::StorageUnavailable(Some(error)),
        StorageError::NotFound => {
            FilesError::Internal(Internal::new("an object isn't in the bucket"))
        }
        StorageError::Failed(error) => FilesError::Internal(error),
    }
}
