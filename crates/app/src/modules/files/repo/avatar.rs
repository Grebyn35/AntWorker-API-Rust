//! The SQL of users' avatars. Every statement filters by the users it is given: the caller, whose
//! row `accounts` has locked, or users that came from a query filtered by tenant (DATA-16).

use platform::{db::UnitOfWork, errors::Internal, storage::ObjectKey, types::UserId};

/// Inserts the avatar of `user`, and returns its ID.
pub(in crate::modules::files) async fn insert(
    uow: &mut UnitOfWork,
    user: UserId,
    media_type: &str,
    size_bytes: u64,
    crc32: u32,
    object_key: &ObjectKey,
) -> Result<uuid::Uuid, Internal> {
    let size_bytes = i64::try_from(size_bytes).map_err(Internal::new)?;
    Ok(sqlx::query_scalar!(
        "INSERT INTO avatars (user_id, media_type, size_bytes, crc32, object_key)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id",
        user.uuid(),
        media_type,
        size_bytes,
        i64::from(crc32),
        object_key.as_str(),
    )
    .fetch_one(uow.executor())
    .await?)
}

/// Deletes the avatar of `user`, and returns its ID, if they had one.
pub(in crate::modules::files) async fn delete(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Option<uuid::Uuid>, Internal> {
    Ok(sqlx::query_scalar!(
        "DELETE FROM avatars WHERE user_id = $1 RETURNING id",
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?)
}

/// The ID of the avatar of each user among `users` that has one.
pub(in crate::modules::files) async fn ids(
    uow: &mut UnitOfWork,
    users: &[UserId],
) -> Result<Vec<(UserId, uuid::Uuid)>, Internal> {
    let users: Vec<uuid::Uuid> = users.iter().map(|user| user.uuid()).collect();
    let rows = sqlx::query!(
        "SELECT user_id, id FROM avatars WHERE user_id = ANY($1)",
        &users,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (UserId::from_uuid(row.user_id), row.id))
        .collect())
}
