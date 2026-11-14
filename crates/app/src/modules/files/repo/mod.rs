//! The SQL of `files`' routes (ARCH-10). Every statement of a workspace's files filters by the
//! workspace the operation's proof names, and every statement of a user's own, in `user.rs`, by
//! the caller (DATA-16). The reads of public files by their ID alone are in `public.rs`, and the
//! periodic jobs that span every workspace and user keep theirs in `jobs.rs`.

pub(super) mod avatar;
pub(super) mod exported;
pub(super) mod picture;
pub(super) mod public;
pub(super) mod user;

use std::time::Duration;

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::{Page, Search},
    storage::ObjectKey,
    types::{Timestamp, UserId, WorkspaceId},
};

use super::model::{FileName, InWorkspace, Recorded, Stored, Visibility};

/// What an upload's row holds besides its owner.
pub(super) struct NewRow<'a> {
    pub(super) name: &'a FileName,
    pub(super) media_type: &'static str,
    pub(super) size_bytes: u64,
    pub(super) crc32: u32,
    pub(super) visibility: Visibility,
    pub(super) object_key: &'a ObjectKey,
}

/// The moment `lifetime` from now, by the database's clock, for a link that a response makes
/// (FILES-021, DATA-03).
pub(super) async fn after(uow: &mut UnitOfWork, lifetime: Duration) -> Result<Timestamp, Internal> {
    let until = sqlx::query_scalar!(
        r#"SELECT now() + make_interval(secs => $1) AS "until!""#,
        lifetime.as_secs_f64(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(Timestamp::from_database(until))
}

/// The columns of a file's row that its record holds besides its owner, as the database gives
/// them: its size as `bigint`, and its visibility as text.
pub(super) struct Columns {
    pub(super) id: uuid::Uuid,
    pub(super) name: String,
    pub(super) media_type: String,
    pub(super) size_bytes: i64,
    pub(super) visibility: String,
    pub(super) created_at: time::OffsetDateTime,
}

impl Columns {
    /// The record of the file whose row holds these columns, of `owner`.
    pub(super) fn recorded<O>(self, owner: O) -> Result<Recorded<O>, Internal> {
        Ok(Recorded {
            id: self.id,
            owner,
            name: FileName::from_stored(self.name),
            media_type: self.media_type,
            size_bytes: u64::try_from(self.size_bytes).map_err(Internal::new)?,
            visibility: Visibility::from_name(&self.visibility)?,
            created_at: Timestamp::from_database(self.created_at),
        })
    }
}

/// Inserts the file that `uploader` uploaded to `workspace`, and returns it.
pub(super) async fn insert(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    uploader: UserId,
    row: &NewRow<'_>,
) -> Result<Recorded<InWorkspace>, Internal> {
    let size_bytes = i64::try_from(row.size_bytes).map_err(Internal::new)?;
    let inserted = sqlx::query!(
        "INSERT INTO workspace_files
            (workspace_id, name, media_type, size_bytes, crc32, visibility, object_key, uploader_id)
        VALUES ($1, $2, $3, $4, $5, $6::text::file_visibility, $7, $8)
        RETURNING id, created_at",
        workspace.uuid(),
        row.name.as_str(),
        row.media_type,
        size_bytes,
        i64::from(row.crc32),
        row.visibility.name(),
        row.object_key.as_str(),
        uploader.uuid(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(Recorded {
        id: inserted.id,
        owner: InWorkspace {
            workspace_id: workspace,
            uploader_id: Some(uploader),
        },
        name: row.name.clone(),
        media_type: row.media_type.to_owned(),
        size_bytes: row.size_bytes,
        visibility: row.visibility,
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
) -> Result<Vec<Recorded<InWorkspace>>, Internal> {
    let rows = sqlx::query!(
        r#"SELECT id, name, media_type, size_bytes, visibility::text AS "visibility!",
            uploader_id, created_at
        FROM workspace_files
        WHERE workspace_id = $1
            AND ($2::text IS NULL OR name COLLATE pg_unicode_fast ILIKE $2)
            AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
        ORDER BY created_at DESC, id DESC
        LIMIT $5"#,
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
            Columns {
                id: row.id,
                name: row.name,
                media_type: row.media_type,
                size_bytes: row.size_bytes,
                visibility: row.visibility,
                created_at: row.created_at,
            }
            .recorded(InWorkspace {
                workspace_id: workspace,
                uploader_id: row.uploader_id.map(UserId::from_uuid),
            })
        })
        .collect()
}

/// The file `id` of `workspace`, with the key of its object, if there is one.
pub(super) async fn read(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    id: uuid::Uuid,
) -> Result<Option<Stored<InWorkspace>>, Internal> {
    let row = sqlx::query!(
        r#"SELECT name, media_type, size_bytes, visibility::text AS "visibility!", object_key,
            uploader_id, created_at
        FROM workspace_files WHERE workspace_id = $1 AND id = $2"#,
        workspace.uuid(),
        id,
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| {
        let owner = InWorkspace {
            workspace_id: workspace,
            uploader_id: row.uploader_id.map(UserId::from_uuid),
        };
        Ok(Stored {
            file: Columns {
                id,
                name: row.name,
                media_type: row.media_type,
                size_bytes: row.size_bytes,
                visibility: row.visibility,
                created_at: row.created_at,
            }
            .recorded(owner)?,
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
