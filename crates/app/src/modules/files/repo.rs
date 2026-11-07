//! The SQL of `files`' routes (ARCH-10). Every statement filters by the workspace the operation's
//! proof names (DATA-16). The periodic jobs that span every workspace keep theirs in `jobs.rs`.

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::{Page, Search},
    storage::ObjectKey,
    types::{Timestamp, UserId, WorkspaceId},
};

use super::model::{File, FileName, Stored};

/// What an upload's row holds besides its workspace and uploader.
pub(super) struct NewRow<'a> {
    pub(super) name: &'a FileName,
    pub(super) media_type: &'static str,
    pub(super) size_bytes: u64,
    pub(super) object_key: &'a ObjectKey,
}

/// Inserts the file that `uploader` uploaded to `workspace`, and returns it.
pub(super) async fn insert(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    uploader: UserId,
    row: &NewRow<'_>,
) -> Result<File, Internal> {
    let size_bytes = i64::try_from(row.size_bytes).map_err(Internal::new)?;
    let inserted = sqlx::query!(
        "INSERT INTO workspace_files (workspace_id, name, media_type, size_bytes, object_key, uploader_id)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id, created_at",
        workspace.uuid(),
        row.name.as_str(),
        row.media_type,
        size_bytes,
        row.object_key.as_str(),
        uploader.uuid(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(File {
        id: inserted.id,
        workspace_id: workspace,
        name: row.name.clone(),
        media_type: row.media_type.to_owned(),
        size_bytes: row.size_bytes,
        uploader_id: Some(uploader),
        created_at: Timestamp::from_database(inserted.created_at),
    })
}

/// A page of `workspace`'s files, the latest uploaded first, kept to those whose name contains
/// `search`, ignoring case in every script, read through the index on the workspace and the
/// upload time (DATA-17, PLATFORM-047).
pub(super) async fn page(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    search: Option<&Search>,
    page: &Page,
) -> Result<Vec<File>, Internal> {
    let rows = sqlx::query!(
        "SELECT id, name, media_type, size_bytes, uploader_id, created_at FROM workspace_files
        WHERE workspace_id = $1
            AND ($2::text IS NULL OR name COLLATE pg_unicode_fast ILIKE $2)
            AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
        ORDER BY created_at DESC, id DESC
        LIMIT $5",
        workspace.uuid(),
        search.map(Search::pattern),
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(File {
                id: row.id,
                workspace_id: workspace,
                name: FileName::from_stored(row.name),
                media_type: row.media_type,
                size_bytes: u64::try_from(row.size_bytes).map_err(Internal::new)?,
                uploader_id: row.uploader_id.map(UserId::from_uuid),
                created_at: Timestamp::from_database(row.created_at),
            })
        })
        .collect()
}

/// The file `id` of `workspace`, with the key of its object, if there is one.
pub(super) async fn read(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    id: uuid::Uuid,
) -> Result<Option<Stored>, Internal> {
    let row = sqlx::query!(
        "SELECT name, media_type, size_bytes, object_key, uploader_id, created_at
        FROM workspace_files WHERE workspace_id = $1 AND id = $2",
        workspace.uuid(),
        id,
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| {
        Ok(Stored {
            file: File {
                id,
                workspace_id: workspace,
                name: FileName::from_stored(row.name),
                media_type: row.media_type,
                size_bytes: u64::try_from(row.size_bytes).map_err(Internal::new)?,
                uploader_id: row.uploader_id.map(UserId::from_uuid),
                created_at: Timestamp::from_database(row.created_at),
            },
            object_key: row.object_key,
        })
    })
    .transpose()
}

/// Deletes the file `id` of `workspace`, and returns whether there was one.
pub(super) async fn delete(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    id: uuid::Uuid,
) -> Result<bool, Internal> {
    let deleted = sqlx::query!(
        "DELETE FROM workspace_files WHERE workspace_id = $1 AND id = $2",
        workspace.uuid(),
        id,
    )
    .execute(uow.executor())
    .await?;
    Ok(deleted.rows_affected() > 0)
}
