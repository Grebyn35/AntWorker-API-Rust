//! The SQL of a user's exports (ARCH-10): `exports`, and its archive's documents in
//! `export_entries` and `export_chunks`. Every statement filters by the user whose
//! export it is, the caller or the user the job runs for, the documents' through their export's
//! row (DATA-16).

use std::{ops::Range, time::Duration};

use platform::{
    archive,
    db::UnitOfWork,
    errors::Internal,
    lists::Page,
    types::{Timestamp, UserId},
};

use crate::modules::accounts::model::{Export, ExportState};

/// The columns of an export's row, as the database gives them.
struct Row {
    id: uuid::Uuid,
    state: String,
    created_at: time::OffsetDateTime,
    ready_at: Option<time::OffsetDateTime>,
    expires_at: time::OffsetDateTime,
    size_bytes: Option<i64>,
}

impl Row {
    fn export(self) -> Result<Export, Internal> {
        Ok(Export {
            id: self.id,
            state: ExportState::from_name(&self.state)?,
            created_at: Timestamp::from_database(self.created_at),
            ready_at: self.ready_at.map(Timestamp::from_database),
            expires_at: Timestamp::from_database(self.expires_at),
            size_bytes: self
                .size_bytes
                .map(u64::try_from)
                .transpose()
                .map_err(Internal::new)?,
        })
    }
}

/// Inserts an export of `user`'s, pending, which expires `lifetime` from now, and returns it
/// (ACCOUNTS-020).
pub(in crate::modules::accounts) async fn insert(
    uow: &mut UnitOfWork,
    user: UserId,
    lifetime: Duration,
) -> Result<Export, Internal> {
    sqlx::query_as!(
        Row,
        r#"INSERT INTO exports (user_id, expires_at)
        VALUES ($1, now() + make_interval(secs => $2))
        RETURNING id, state::text AS "state!", created_at, ready_at, expires_at, size_bytes"#,
        user.uuid(),
        lifetime.as_secs_f64(),
    )
    .fetch_one(uow.executor())
    .await?
    .export()
}

/// A page of `user`'s exports that haven't expired, the latest first, through the index on the
/// user and the request's time (ACCOUNTS-022, DATA-17).
pub(in crate::modules::accounts) async fn page(
    uow: &mut UnitOfWork,
    user: UserId,
    page: &Page,
) -> Result<Vec<Export>, Internal> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, state::text AS "state!", created_at, ready_at, expires_at, size_bytes
        FROM exports
        WHERE user_id = $1
            AND expires_at > now()
            AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4"#,
        user.uuid(),
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?
    .into_iter()
    .map(Row::export)
    .collect()
}

/// The export `id` of `user`'s, unless it is gone or has expired (ACCOUNTS-022).
pub(in crate::modules::accounts) async fn read(
    uow: &mut UnitOfWork,
    user: UserId,
    id: uuid::Uuid,
) -> Result<Option<Export>, Internal> {
    sqlx::query_as!(
        Row,
        r#"SELECT id, state::text AS "state!", created_at, ready_at, expires_at, size_bytes
        FROM exports
        WHERE id = $1 AND user_id = $2 AND expires_at > now()"#,
        id,
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?
    .map(Row::export)
    .transpose()
}

/// Locks the export `id` of `user`'s, `FOR UPDATE`, and returns its state, unless it is gone
/// (ACCOUNTS-023).
pub(in crate::modules::accounts) async fn lock(
    uow: &mut UnitOfWork,
    user: UserId,
    id: uuid::Uuid,
) -> Result<Option<ExportState>, Internal> {
    let state = sqlx::query_scalar!(
        r#"SELECT state::text AS "state!" FROM exports
        WHERE id = $1 AND user_id = $2
        FOR UPDATE"#,
        id,
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    state.as_deref().map(ExportState::from_name).transpose()
}

/// Makes the export `id` of `user`'s ready, now, with an archive of `size_bytes` bytes, expiring
/// `lifetime` from now, and returns it (ACCOUNTS-023).
pub(in crate::modules::accounts) async fn ready(
    uow: &mut UnitOfWork,
    user: UserId,
    id: uuid::Uuid,
    size_bytes: u64,
    lifetime: Duration,
) -> Result<Export, Internal> {
    let size_bytes = i64::try_from(size_bytes).map_err(Internal::new)?;
    sqlx::query_as!(
        Row,
        r#"UPDATE exports
        SET state = 'ready', ready_at = now(), size_bytes = $3,
            expires_at = now() + make_interval(secs => $4)
        WHERE id = $1 AND user_id = $2
        RETURNING id, state::text AS "state!", created_at, ready_at, expires_at, size_bytes"#,
        id,
        user.uuid(),
        size_bytes,
        lifetime.as_secs_f64(),
    )
    .fetch_one(uow.executor())
    .await?
    .export()
}

/// Makes the export `id` of `user`'s failed while it is pending, and returns whether it did
/// (ACCOUNTS-023).
pub(in crate::modules::accounts) async fn fail(
    uow: &mut UnitOfWork,
    user: UserId,
    id: uuid::Uuid,
) -> Result<bool, Internal> {
    let failed = sqlx::query!(
        "UPDATE exports SET state = 'failed'
        WHERE id = $1 AND user_id = $2 AND state = 'pending'",
        id,
        user.uuid(),
    )
    .execute(uow.executor())
    .await?;
    Ok(failed.rows_affected() > 0)
}

/// Stores `bytes`, a piece of the deflated bytes of the document at `position` of the export
/// `export`, whose row the job of `user`'s export locked, at `offset` among them (PLATFORM-084).
pub(in crate::modules::accounts) async fn insert_piece(
    uow: &mut UnitOfWork,
    user: UserId,
    export: uuid::Uuid,
    position: u32,
    offset: u64,
    bytes: &[u8],
) -> Result<(), Internal> {
    let offset = i64::try_from(offset).map_err(Internal::new)?;
    sqlx::query!(
        "INSERT INTO export_chunks (export_id, position, offset_bytes, bytes)
        SELECT id, $3, $4, $5 FROM exports WHERE id = $1 AND user_id = $2",
        export,
        user.uuid(),
        i32::try_from(position).map_err(Internal::new)?,
        offset,
        bytes,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Stores `entry`, the document at `position` of the export `export` of `user`'s.
pub(in crate::modules::accounts) async fn insert_entry(
    uow: &mut UnitOfWork,
    user: UserId,
    export: uuid::Uuid,
    position: u32,
    entry: &archive::Entry,
) -> Result<(), Internal> {
    sqlx::query!(
        "INSERT INTO export_entries (export_id, position, path, size_bytes, stored_bytes, crc32)
        SELECT id, $3, $4, $5, $6, $7 FROM exports WHERE id = $1 AND user_id = $2",
        export,
        user.uuid(),
        i32::try_from(position).map_err(Internal::new)?,
        entry.path,
        i64::try_from(entry.size).map_err(Internal::new)?,
        i64::try_from(entry.stored_size).map_err(Internal::new)?,
        i64::from(entry.crc32),
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// The documents of the export `export` of `user`'s, in their order, each with its position, as
/// the archive's entries.
pub(in crate::modules::accounts) async fn entries(
    uow: &mut UnitOfWork,
    user: UserId,
    export: uuid::Uuid,
) -> Result<Vec<(u32, archive::Entry)>, Internal> {
    let rows = sqlx::query!(
        "SELECT entry.position, entry.path, entry.size_bytes, entry.stored_bytes, entry.crc32
        FROM export_entries AS entry
        JOIN exports AS export ON export.id = entry.export_id
        WHERE entry.export_id = $1 AND export.user_id = $2
        ORDER BY entry.position",
        export,
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            let entry = archive::Entry {
                path: row.path,
                size: u64::try_from(row.size_bytes).map_err(Internal::new)?,
                stored_size: u64::try_from(row.stored_bytes).map_err(Internal::new)?,
                crc32: u32::try_from(row.crc32).map_err(Internal::new)?,
                deflated: true,
            };
            Ok((u32::try_from(row.position).map_err(Internal::new)?, entry))
        })
        .collect()
}

/// The pieces of the deflated bytes of the document at `position` of the export `export` of
/// `user`'s that hold any of `offsets`, at most `limit` of them, each with its offset, in their
/// order.
pub(in crate::modules::accounts) async fn pieces(
    uow: &mut UnitOfWork,
    user: UserId,
    export: uuid::Uuid,
    position: u32,
    offsets: Range<u64>,
    limit: i64,
) -> Result<Vec<(u64, Vec<u8>)>, Internal> {
    let rows = sqlx::query!(
        "SELECT chunk.offset_bytes, chunk.bytes
        FROM export_chunks AS chunk
        JOIN exports AS export ON export.id = chunk.export_id
        WHERE chunk.export_id = $1 AND export.user_id = $2 AND chunk.position = $3
            AND chunk.offset_bytes < $5
            AND chunk.offset_bytes + octet_length(chunk.bytes) > $4
        ORDER BY chunk.offset_bytes
        LIMIT $6",
        export,
        user.uuid(),
        i32::try_from(position).map_err(Internal::new)?,
        i64::try_from(offsets.start).map_err(Internal::new)?,
        i64::try_from(offsets.end).map_err(Internal::new)?,
        limit,
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                u64::try_from(row.offset_bytes).map_err(Internal::new)?,
                row.bytes,
            ))
        })
        .collect()
}
