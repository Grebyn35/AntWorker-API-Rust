//! Uploading, listing, reading, downloading and deleting files (the files spec): what a
//! workspace's files and a user's own share, here, and each one's operations, in `workspace.rs`
//! and `user.rs`, with the public downloads' reads in `public.rs`, users' avatars in `avatar.rs`
//! and workspaces' pictures in `picture.rs`.

pub(super) mod avatar;
pub(super) mod exported;
pub(super) mod picture;
pub(super) mod public;
pub(super) mod user;
pub(super) mod workspace;

use std::{ops::Range, time::Duration};

use bytes::Bytes;
use platform::{
    archive,
    blocking::Workers,
    db::Database,
    db::UnitOfWork,
    errors::Internal,
    idempotency::IdempotencyKey,
    ranges::{Served, Wanted},
    scanner::{ScanError, Scanner, Verdict},
    signed_urls::SignedUrls,
    storage::{ObjectBytes, ObjectKey, Storage, StorageError},
    types::{SessionId, Timestamp},
};

use super::{
    error::FilesError,
    metrics::{self, Upload},
    model::{FileName, InWorkspace, Kind, Link, OfUser, Recorded, Settings, Visibility},
    repo,
};

/// How long a private file's signed link lasts: an hour, the access token's default lifetime,
/// fixed, since it bounds what a leaked link gives (FILES-021, the files spec's "Links").
const LINK_LIFETIME: Duration = Duration::from_secs(3_600);

/// When a link that a response makes in `uow` stops working (FILES-021).
pub(super) async fn link_expiry(uow: &mut UnitOfWork) -> Result<Timestamp, FilesError> {
    Ok(repo::after(uow, LINK_LIFETIME).await?)
}

/// The prefix of the keys of workspaces' files in the bucket.
pub(super) const WORKSPACE_PREFIX: &str = "workspaces/";

/// The prefix of the keys of users' own files in the bucket.
pub(super) const USER_PREFIX: &str = "users/";

/// What the routes and jobs of `files` share: the database, the bucket, the malware scanner,
/// the turns of the instance's CPU-heavy work, which reading and checking an upload's bytes take,
/// the signer of the downloads' links, and the module's settings.
#[derive(Clone)]
pub(crate) struct Files {
    pub(super) database: Database,
    pub(super) storage: Storage,
    pub(super) scanner: Scanner,
    pub(super) workers: Workers,
    pub(super) signed: SignedUrls,
    pub(super) settings: Settings,
}

impl Files {
    /// The module's state, on `database`, `storage`, `scanner` and `workers`, signing links with
    /// `signed`.
    pub(crate) fn new(
        database: Database,
        storage: Storage,
        scanner: Scanner,
        workers: Workers,
        signed: SignedUrls,
        settings: Settings,
    ) -> Self {
        Self {
            database,
            storage,
            scanner,
            workers,
            signed,
            settings,
        }
    }

    /// The links of the workspace's file `file`, for `session`, a private one's signed until
    /// `until` (FILES-021).
    pub(super) fn workspace_link(
        &self,
        file: &Recorded<InWorkspace>,
        session: SessionId,
        until: Timestamp,
    ) -> Link {
        self.link(
            file.visibility,
            &format!("/v1/public/workspace-files/{}", file.id),
            &format!(
                "/v1/workspaces/{}/files/{}/content",
                file.owner.workspace_id, file.id
            ),
            session,
            until,
        )
    }

    /// The links of the user's own file `file`, for `session`, a private one's signed until
    /// `until` (FILES-021).
    pub(super) fn user_link(
        &self,
        file: &Recorded<OfUser>,
        session: SessionId,
        until: Timestamp,
    ) -> Link {
        self.link(
            file.visibility,
            &format!("/v1/public/user-files/{}", file.id),
            &format!("/v1/users/me/files/{}/content", file.id),
            session,
            until,
        )
    }

    /// `public`, for a public file, or `private` signed for `session` until `until`.
    fn link(
        &self,
        visibility: Visibility,
        public: &str,
        private: &str,
        session: SessionId,
        until: Timestamp,
    ) -> Link {
        match visibility {
            Visibility::Public => Link {
                content_url: public.to_owned(),
                content_url_expires_at: None,
            },
            Visibility::Private => {
                let signed = self.signed.sign(private, session, until);
                Link {
                    content_url: signed.url,
                    content_url_expires_at: Some(signed.expires_at),
                }
            }
        }
    }
}

/// What an upload's request sent: the file's name and visibility, its bytes, and its
/// `Idempotency-Key`, if any.
pub(super) struct Sent<'a> {
    pub(super) name: FileName,
    pub(super) visibility: Visibility,
    pub(super) bytes: Bytes,
    pub(super) key: Option<&'a IdempotencyKey>,
}

/// An upload whose kind is checked and whose bytes the scanner found clean, with the key its
/// object was written under.
pub(super) struct Written {
    pub(super) name: FileName,
    pub(super) kind: Kind,
    pub(super) object: Object,
}

/// An upload's object, once written: the size and CRC-32 of its bytes, and its key (FILES-025).
pub(super) struct Object {
    pub(super) size_bytes: u64,
    pub(super) crc32: u32,
    pub(super) key: ObjectKey,
}

/// Uploads the file `name`, with `bytes`, under `prefix`: checks its kind, has the scanner scan
/// its bytes, writes its object outside any transaction, then has `record` record it, in a unit
/// of work of its own (FILES-003, FILES-004, FILES-005, DATA-21). An object whose row never
/// commits is left to the deletion of unreferenced objects, never deleted here: when a commit's
/// answer is lost, the row may exist (FILES-011). The upload counts once in `files.uploads`, by
/// how it ended (FILES-013).
pub(super) async fn upload<T, R>(
    files: &Files,
    prefix: &str,
    name: FileName,
    bytes: Bytes,
    record: impl FnOnce(Written) -> R,
) -> Result<T, FilesError>
where
    R: Future<Output = Result<T, FilesError>>,
{
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
    store(files, counted, prefix, kind, bytes, async move |object| {
        record(Written { name, kind, object }).await
    })
    .await
}

/// An image that came with no name, an avatar or a workspace's picture, whose bytes are checked
/// and scanned, and whose object is written (the files spec's "Avatars").
pub(super) struct Image {
    pub(super) kind: Kind,
    pub(super) object: Object,
}

/// Uploads an image that came with no name, under `prefix`: finds the kind of its `bytes` among
/// the image kinds, from the bytes alone; has the scanner scan them; writes their object outside
/// any transaction; then has `record` record it, in a unit of work of its caller's (D121,
/// DATA-21). An object whose row never commits is left to the deletion of unreferenced objects
/// (FILES-011), and the upload counts once in `files.uploads`, by how it ended (FILES-013).
pub(super) async fn upload_image<T, E, R>(
    files: &Files,
    prefix: &str,
    bytes: Bytes,
    record: impl FnOnce(Image) -> R,
) -> Result<T, E>
where
    R: Future<Output = Result<T, E>>,
    E: From<FilesError>,
{
    if bytes.is_empty() {
        return Err(FilesError::Empty.into());
    }
    let counted = metrics::Counted::start();
    // An image kind's check compares the bytes' first few with its signature, which is no
    // CPU-heavy work, unlike a text's.
    let Some(kind) = Kind::image_of(&bytes) else {
        counted.end(Upload::UnsupportedType);
        return Err(FilesError::UnsupportedType.into());
    };
    store(files, counted, prefix, kind, bytes, |object| {
        record(Image { kind, object })
    })
    .await
}

/// Has the scanner scan `bytes`, of `kind`, writes them under `prefix`, outside any transaction,
/// then has `record` record them, with their size, their CRC-32, which the workers compute, and
/// their object's key, and counts the upload that `counted` started by how it ended: stored once
/// `record` succeeds (FILES-005, FILES-013, FILES-025, DATA-21). An image's upload shares it with
/// a file's.
pub(super) async fn store<T, E, R>(
    files: &Files,
    counted: metrics::Counted,
    prefix: &str,
    kind: Kind,
    bytes: Bytes,
    record: impl FnOnce(Object) -> R,
) -> Result<T, E>
where
    R: Future<Output = Result<T, E>>,
    E: From<FilesError>,
{
    match files.scanner.scan(&bytes).await {
        Ok(Verdict::Clean) => {}
        Ok(Verdict::Found(_)) => {
            counted.end(Upload::MalwareDetected);
            return Err(FilesError::MalwareDetected.into());
        }
        Err(error) => {
            counted.end(Upload::Failed);
            return Err(match error {
                ScanError::Unavailable(cause) => FilesError::ScannerUnavailable(Some(cause)),
                ScanError::Failed(cause) => FilesError::Internal(cause),
            }
            .into());
        }
    }
    let stored = async {
        let size_bytes =
            u64::try_from(bytes.len()).map_err(|error| FilesError::from(Internal::new(error)))?;
        // A checksum reads every byte, so it is CPU-heavy work (CODE-05).
        let summed = bytes.clone();
        let crc32 = files
            .workers
            .run("upload checksum", move || archive::crc32(&summed))
            .await
            .map_err(|error| FilesError::from(Internal::new(error)))?;
        let key = ObjectKey::random(prefix).map_err(FilesError::from)?;
        files
            .storage
            .put(&key, bytes, kind.media_type())
            .await
            .map_err(unavailable)?;
        record(Object {
            size_bytes,
            crc32,
            key,
        })
        .await
    }
    .await;
    counted.end(if stored.is_ok() {
        Upload::Stored
    } else {
        Upload::Failed
    });
    stored
}

/// The file a download serves: its ID, its media type and size, the key of its object, the name
/// it is saved under, and whether it is a public download, which shared caches may keep and which
/// gives no file's own name (FILES-009, FILES-023).
pub(super) struct Serving {
    pub(super) id: uuid::Uuid,
    pub(super) media_type: String,
    pub(super) size_bytes: u64,
    pub(super) object_key: String,
    pub(super) name: String,
    pub(super) public: bool,
}

/// What a download serves of a file: the file, what of its bytes `Wanted` asked for, and those
/// bytes, as the bucket streams them, or none when the client holds them (PLATFORM-081).
pub(crate) struct Downloaded {
    pub(super) file: Serving,
    pub(super) served: Served,
    pub(super) bytes: Option<ObjectBytes>,
}

/// What a download of `file` serves, as `wanted` asks for it: nothing when the client holds its
/// bytes, as `If-None-Match` says, and otherwise its bytes, or the range of them that the request
/// names, from the bucket (FILES-009, FILES-023, PLATFORM-081).
async fn serve(files: &Files, file: Serving, wanted: &Wanted) -> Result<Downloaded, FilesError> {
    let served = wanted.serve(file.size_bytes, &file.id.to_string());
    let bytes = if served == Served::NotModified {
        None
    } else {
        Some(bytes(files, &file, served.offsets()).await?)
    };
    Ok(Downloaded {
        file,
        served,
        bytes,
    })
}

/// The bytes of `file`, or those of `range` alone, offsets of its bytes, as the bucket streams
/// them, once the bucket's answer shows its object at the row's size (FILES-009). A row whose
/// object the bucket doesn't hold, or holds at another size, is an internal error, whose cause
/// names the file, since only a fault of the bucket's leaves one (ERR-10).
async fn bytes(
    files: &Files,
    file: &Serving,
    range: Option<Range<u64>>,
) -> Result<ObjectBytes, FilesError> {
    let (id, size_bytes) = (file.id, file.size_bytes);
    match files
        .storage
        .get(&ObjectKey::from_stored(&file.object_key), range)
        .await
    {
        Ok(object) if object.size == size_bytes => Ok(object.bytes),
        // An object of another size isn't the file's bytes, as one cut short isn't.
        Ok(object) => Err(FilesError::Internal(Internal::new(format!(
            "the row of the file {id} names an object of {size_bytes} bytes, which the bucket holds with {}",
            object.size
        )))),
        Err(StorageError::NotFound) => Err(FilesError::Internal(Internal::new(format!(
            "the row of the file {id} names an object that the bucket doesn't hold"
        )))),
        // A range that the row's size holds and the object doesn't, as when its bytes were cut
        // short, is one way the bucket refuses a read, whose cause names the file too.
        Err(StorageError::Failed(cause)) => {
            Err(FilesError::Internal(Internal::new(Refused { id, cause })))
        }
        Err(error) => Err(unavailable(error)),
    }
}

/// A read of a file's bytes that the bucket refused, whose cause names the file and keeps the
/// bucket's own as its source (FILES-009, ERR-10).
#[derive(Debug, thiserror::Error)]
#[error("the bucket refused the read of the bytes of the file {id}")]
struct Refused {
    id: uuid::Uuid,
    #[source]
    cause: Internal,
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
