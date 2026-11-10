//! The SQL of `verification_codes` (ARCH-10): a code is one user's for one purpose, found by both
//! (DATA-16), and expires by the database's clock (DATA-03). An account holds one code that proves
//! its address at a time, and a reset code for each request (AUTH-036). A row is never edited
//! (DATA-19).

use platform::{db::UnitOfWork, errors::Internal, types::UserId};

use crate::modules::auth::model::Purpose;

/// A user's code for a purpose, as a check of it reads it.
pub(in crate::modules::auth) struct StoredCode {
    pub(in crate::modules::auth) id: uuid::Uuid,
    /// The HMAC of the code (SEC-03).
    pub(in crate::modules::auth) hash: Vec<u8>,
    /// Whether its time has passed, by the database's clock.
    pub(in crate::modules::auth) expired: bool,
}

/// Inserts `user_id`'s code that proves their address, whose HMAC is `hash`, lasting `minutes`
/// from now, in place of the one before it, and returns its ID; or `None` when another unit of work
/// issued one meanwhile, which the unique index `verification_codes_user_id_key` refuses without
/// aborting the unit of work (AUTH-030, ERR-05).
pub(in crate::modules::auth) async fn replace(
    uow: &mut UnitOfWork,
    user_id: UserId,
    hash: &[u8],
    minutes: i32,
) -> Result<Option<uuid::Uuid>, Internal> {
    sqlx::query!(
        "DELETE FROM verification_codes WHERE user_id = $1 AND purpose = 'email_verification'",
        user_id.uuid(),
    )
    .execute(uow.executor())
    .await?;
    Ok(sqlx::query_scalar!(
        "INSERT INTO verification_codes (user_id, purpose, code_hash, expires_at)
        VALUES ($1, 'email_verification', $2, now() + make_interval(mins => $3))
        ON CONFLICT (user_id) WHERE purpose = 'email_verification' DO NOTHING
        RETURNING id",
        user_id.uuid(),
        hash,
        minutes,
    )
    .fetch_optional(uow.executor())
    .await?)
}

/// Inserts a reset code of `user_id`'s, whose HMAC is `hash`, lasting `minutes` from now, beside
/// any other that lasts, and returns its ID (AUTH-036).
pub(in crate::modules::auth) async fn insert_reset(
    uow: &mut UnitOfWork,
    user_id: UserId,
    hash: &[u8],
    minutes: i32,
) -> Result<uuid::Uuid, Internal> {
    Ok(sqlx::query_scalar!(
        "INSERT INTO verification_codes (user_id, purpose, code_hash, expires_at)
        VALUES ($1, 'password_reset', $2, now() + make_interval(mins => $3))
        RETURNING id",
        user_id.uuid(),
        hash,
        minutes,
    )
    .fetch_one(uow.executor())
    .await?)
}

/// `user_id`'s codes for `purpose`: one at most that proves their address, and their reset codes,
/// the oldest first.
pub(in crate::modules::auth) async fn find(
    uow: &mut UnitOfWork,
    user_id: UserId,
    purpose: Purpose,
) -> Result<Vec<StoredCode>, Internal> {
    let rows = sqlx::query!(
        r#"SELECT id, code_hash, expires_at <= now() AS "expired!" FROM verification_codes
        WHERE user_id = $1 AND purpose = $2::text::verification_purpose
        ORDER BY id"#,
        user_id.uuid(),
        purpose.name(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| StoredCode {
            id: row.id,
            hash: row.code_hash,
            expired: row.expired,
        })
        .collect())
}

/// Deletes every reset code of `user_id`, once a reset used one, so none resets again
/// (AUTH-037).
pub(in crate::modules::auth) async fn delete_resets(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM verification_codes WHERE user_id = $1 AND purpose = 'password_reset'",
        user_id.uuid(),
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Deletes `user_id`'s code `id`, which a check just matched, so it proves nothing again, and
/// says whether it did: a row only one unit of work deletes, so a code that a newer one replaced
/// meanwhile proves nothing (AUTH-031, AUTH-032).
pub(in crate::modules::auth) async fn claim(
    uow: &mut UnitOfWork,
    user_id: UserId,
    id: uuid::Uuid,
) -> Result<bool, Internal> {
    let deleted = sqlx::query!(
        "DELETE FROM verification_codes WHERE user_id = $1 AND id = $2",
        user_id.uuid(),
        id,
    )
    .execute(uow.executor())
    .await?
    .rows_affected();
    Ok(deleted == 1)
}

/// Deletes every code of `user_id`, whose account the unit of work deletes (ACCOUNTS-013).
pub(in crate::modules::auth) async fn delete_all(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM verification_codes WHERE user_id = $1",
        user_id.uuid()
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}
