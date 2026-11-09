//! What an export holds of files (FILES-026, the accounts spec's "Exporting"): the contract's
//! part of the archive, whose documents give each file's record and the path of its bytes, and
//! whose rows of `exported_files` hold those bytes for as long as the export lasts; and what
//! `accounts` reads to serve the archive: those files, and their bytes from the bucket.

use std::ops::Range;

use platform::{
    db::UnitOfWork,
    errors::Internal,
    personal_data::{ExportArchive, next_after},
    storage::{ObjectBytes, ObjectKey},
    types::{Timestamp, UserId, WorkspaceId},
};

use super::Files;
use crate::modules::{
    access::{self, AccessError},
    files::{
        model::{
            AvatarRecord, ExportedFile, FileName, Kind, OwnRecord, Visibility, WorkspaceRecord,
            path_segment,
        },
        repo::exported::{self as repo, Row},
    },
};

/// The files whose bytes `user`'s export `export` holds, in their order, which `accounts` lays
/// out after the export's documents.
pub(crate) async fn exported(
    uow: &mut UnitOfWork,
    user: UserId,
    export: uuid::Uuid,
) -> Result<Vec<ExportedFile>, Internal> {
    repo::held(uow, user, export).await
}

/// The bytes at `offsets` of `file`, which an export holds, as the bucket streams them, once the
/// bucket's answer shows its object at the size its row gave (FILES-009). A download's response
/// has started by then, so any failure, of the bucket's own or of an object of another size, ends
/// its body.
pub(crate) async fn exported_bytes(
    files: &Files,
    file: &ExportedFile,
    offsets: Range<u64>,
) -> Result<ObjectBytes, Internal> {
    let object = files
        .storage
        .get(&ObjectKey::from_stored(&file.object_key), Some(offsets))
        .await
        .map_err(Internal::new)?;
    if object.size != file.size_bytes {
        return Err(Internal::new(format!(
            "an exported file of {} bytes names an object of {} bytes",
            file.size_bytes, object.size
        )));
    }
    Ok(object.bytes)
}

/// Writes the contract's part of `export`: the user's own files, then their avatar, then the
/// files of each workspace whose only member they are, each kind a document of its records, a
/// page at a time, with a row of `exported_files` for each file, which holds its bytes (FILES-026).
pub(in crate::modules::files) async fn export(
    uow: &mut UnitOfWork,
    export: &mut ExportArchive,
) -> Result<(), Internal> {
    let user = export.user();
    let mut held = Held::default();
    let mut document = export.document("files/files.json");
    let mut after = None;
    loop {
        let page = repo::own(uow, user, after).await?;
        let mut records = Vec::with_capacity(page.len());
        for row in &page {
            let path = format!("files/own/{}/{}", row.id, path_segment(&name(row)));
            records.push(OwnRecord {
                id: row.id,
                name: name(row),
                media_type: row.media_type.clone(),
                size_bytes: size(row)?,
                visibility: Visibility::from_name(&row.visibility)?,
                created_at: Timestamp::from_database(row.created_at),
                path: path.clone(),
            });
            held.add(row, path)?;
        }
        export.write(uow, &mut document, &records).await?;
        held.store(uow, export).await?;
        match next_after(&page) {
            Some(last) => after = Some((last.created_at, last.id)),
            None => break,
        }
    }
    export.finish(uow, document).await?;
    let mut records = Vec::new();
    if let Some(row) = repo::avatar(uow, user).await? {
        let extension = Kind::of_media_type(&row.media_type)
            .ok_or_else(|| Internal::new("an avatar of a kind that files doesn't know"))?
            .extension();
        let path = format!("files/avatar/{}.{extension}", row.id);
        records.push(AvatarRecord {
            id: row.id,
            media_type: row.media_type.clone(),
            size_bytes: size(&row)?,
            created_at: Timestamp::from_database(row.created_at),
            path: path.clone(),
        });
        held.add(&row, path)?;
    }
    export.whole(uow, "files/avatar.json", &records).await?;
    held.store(uow, export).await?;
    let sole = access::sole_workspaces(uow, user)
        .await
        .map_err(|error| match error {
            AccessError::Internal(error) => error,
        })?;
    let mut document = export.document("files/workspace_files.json");
    for workspace in sole {
        workspace_files(uow, export, &mut document, &mut held, workspace).await?;
    }
    export.finish(uow, document).await
}

/// Writes into `document` every file of `workspace`, a page at a time, holding their bytes.
async fn workspace_files(
    uow: &mut UnitOfWork,
    export: &ExportArchive,
    document: &mut platform::personal_data::Document,
    held: &mut Held,
    workspace: WorkspaceId,
) -> Result<(), Internal> {
    let mut after = None;
    loop {
        let page = repo::of_workspace(uow, workspace, after).await?;
        let mut records = Vec::with_capacity(page.len());
        for row in &page {
            let path = format!(
                "files/workspaces/{}/{}/{}",
                workspace.uuid(),
                row.id,
                path_segment(&name(row))
            );
            records.push(WorkspaceRecord {
                id: row.id,
                workspace_id: workspace,
                name: name(row),
                media_type: row.media_type.clone(),
                size_bytes: size(row)?,
                visibility: Visibility::from_name(&row.visibility)?,
                uploader_id: row.uploader_id.map(UserId::from_uuid),
                created_at: Timestamp::from_database(row.created_at),
                path: path.clone(),
            });
            held.add(row, path)?;
        }
        export.write(uow, document, &records).await?;
        held.store(uow, export).await?;
        match next_after(&page) {
            Some(last) => after = Some((last.created_at, last.id)),
            None => return Ok(()),
        }
    }
}

/// The files of a page whose rows of `exported_files` aren't stored yet, and the position of the
/// first of them among every file the export holds.
#[derive(Default)]
struct Held {
    next: u32,
    files: Vec<ExportedFile>,
}

impl Held {
    /// Adds the file of `row`, whose bytes are at `path` in the archive.
    fn add(&mut self, row: &Row, path: String) -> Result<(), Internal> {
        self.files.push(ExportedFile {
            path,
            object_key: row.object_key.clone(),
            size_bytes: size(row)?,
            crc32: u32::try_from(row.crc32).map_err(Internal::new)?,
        });
        Ok(())
    }

    /// Stores the rows of the files added since the last time, at their positions.
    async fn store(
        &mut self,
        uow: &mut UnitOfWork,
        export: &ExportArchive,
    ) -> Result<(), Internal> {
        if self.files.is_empty() {
            return Ok(());
        }
        repo::hold(uow, export.user(), export.id(), self.next, &self.files).await?;
        self.next += u32::try_from(self.files.len()).map_err(Internal::new)?;
        self.files.clear();
        Ok(())
    }
}

/// The name of `row`'s file, which was normalized when it was stored.
fn name(row: &Row) -> FileName {
    FileName::from_stored(row.name.clone())
}

/// The size of `row`'s file.
fn size(row: &Row) -> Result<u64, Internal> {
    u64::try_from(row.size_bytes).map_err(Internal::new)
}
