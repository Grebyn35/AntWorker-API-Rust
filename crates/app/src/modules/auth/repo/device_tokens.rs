//! The SQL of `device_tokens` (ARCH-10): a token is found by its hash, through its unique index,
//! with the HMAC of the email it was issued for, and expires by the database's clock (DATA-03,
//! DATA-16, AUTH-019).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{Timestamp, UserId},
};

/// Whether the device token that hashes to `token` is one of the email whose HMAC is `email`, and
/// hasn't expired (AUTH-019).
pub(in crate::modules::auth) async fn device_token_valid(
    uow: &mut UnitOfWork,
    token: &[u8],
    email: &[u8],
) -> Result<bool, Internal> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
            SELECT 1 FROM device_tokens
            WHERE token_hash = $1 AND email_hash = $2 AND expires_at > now()
        ) AS "valid!""#,
        token,
        email,
    )
    .fetch_one(uow.executor())
    .await?)
}

/// Deletes `user_id`'s expired device tokens, and the one that hashes to `token`, if any.
pub(in crate::modules::auth) async fn end_device_tokens(
    uow: &mut UnitOfWork,
    user_id: UserId,
    token: Option<&[u8]>,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM device_tokens
        WHERE user_id = $1 AND (expires_at <= now() OR token_hash = $2)",
        user_id.uuid(),
        token,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Inserts `user_id`'s device token that hashes to `token`, for the email whose HMAC is `email`,
/// lasting `lifetime`, and returns its ID and when it expires.
pub(in crate::modules::auth) async fn insert_device_token(
    uow: &mut UnitOfWork,
    user_id: UserId,
    email: &[u8],
    token: &[u8],
    lifetime: std::time::Duration,
) -> Result<(uuid::Uuid, Timestamp), Internal> {
    let row = sqlx::query!(
        "INSERT INTO device_tokens (user_id, email_hash, token_hash, expires_at)
        VALUES ($1, $2, $3, now() + make_interval(secs => $4::float8))
        RETURNING id, expires_at",
        user_id.uuid(),
        email,
        token,
        lifetime.as_secs_f64(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok((row.id, Timestamp::from_database(row.expires_at)))
}

/// Deletes every device token of `user_id`, whose account the unit of work deletes
/// (ACCOUNTS-013), or whose address it changes (AUTH-051).
pub(in crate::modules::auth) async fn delete_device_tokens(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM device_tokens WHERE user_id = $1",
        user_id.uuid()
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}
