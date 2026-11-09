//! The SQL of a user's own files. Every statement filters by the user the request's `Principal`
//! proves (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::{Page, Search},
    types::{Timestamp, UserId},
};

use super::{Columns, NewRow};
use crate::modules::files::model::{OfUser, Recorded, Stored};

/// Inserts the file that `user` uploaded to their own files, and returns it.
pub(in crate::modules::files) async fn insert(
    uow: &mut UnitOfWork,
    user: UserId,
    row: &NewRow<'_>,
) -> Result<Recorded<OfUser>, Internal> {
    let size_bytes = i64::try_from(row.size_bytes).map_err(Internal::new)?;
    let inserted = sqlx::query!(
        "INSERT INTO user_files (user_id, name, media_type, size_bytes, crc32, visibility, object_key)
        VALUES ($1, $2, $3, $4, $5, $6::text::file_visibility, $7)
        RETURNING id, created_at",
        user.uuid(),
        row.name.as_str(),
        row.media_type,
        size_bytes,
        i64::from(row.crc32),
        row.visibility.name(),
        row.object_key.as_str(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(Recorded {
        id: inserted.id,
        owner: OfUser { user_id: user },
        name: row.name.clone(),
        media_type: row.media_type.to_owned(),
        size_bytes: row.size_bytes,
        visibility: row.visibility,
        created_at: Timestamp::from_database(inserted.created_at),
    })
}

/// A page of `user`'s own files, the latest uploaded first, kept to those whose name contains
/// `search`, ignoring case in every script, read through the index on the user and the upload
/// time (DATA-17, PLATFORM-047).
pub(in crate::modules::files) async fn page(
    uow: &mut UnitOfWork,
    user: UserId,
    search: Option<&Search>,
    page: &Page,
) -> Result<Vec<Recorded<OfUser>>, Internal> {
    let rows = sqlx::query!(
        r#"SELECT id, name, media_type, size_bytes, visibility::text AS "visibility!", created_at
        FROM user_files
        WHERE user_id = $1
            AND ($2::text IS NULL OR name COLLATE pg_unicode_fast ILIKE $2)
            AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
        ORDER BY created_at DESC, id DESC
        LIMIT $5"#,
        user.uuid(),
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
            .recorded(OfUser { user_id: user })
        })
        .collect()
}

/// The file `id` of `user`'s own, with the key of its object, if there is one.
pub(in crate::modules::files) async fn read(
    uow: &mut UnitOfWork,
    user: UserId,
    id: uuid::Uuid,
) -> Result<Option<Stored<OfUser>>, Internal> {
    let row = sqlx::query!(
        r#"SELECT name, media_type, size_bytes, visibility::text AS "visibility!", object_key,
            created_at
        FROM user_files WHERE user_id = $1 AND id = $2"#,
        user.uuid(),
        id,
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| {
        Ok(Stored {
            file: Columns {
                id,
                name: row.name,
                media_type: row.media_type,
                size_bytes: row.size_bytes,
                visibility: row.visibility,
                created_at: row.created_at,
            }
            .recorded(OfUser { user_id: user })?,
            object_key: row.object_key,
        })
    })
    .transpose()
}

/// Deletes the file `id` of `user`'s own, and returns whether there was one.
pub(in crate::modules::files) async fn delete(
    uow: &mut UnitOfWork,
    user: UserId,
    id: uuid::Uuid,
) -> Result<bool, Internal> {
    let deleted = sqlx::query!(
        "DELETE FROM user_files WHERE user_id = $1 AND id = $2",
        user.uuid(),
        id,
    )
    .execute(uow.executor())
    .await?;
    Ok(deleted.rows_affected() > 0)
}

/// Deletes every file of `user`'s own, whose account the unit of work deletes (ACCOUNTS-013).
pub(in crate::modules::files) async fn delete_all(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<(), Internal> {
    sqlx::query!("DELETE FROM user_files WHERE user_id = $1", user.uuid())
        .execute(uow.executor())
        .await?;
    Ok(())
}
