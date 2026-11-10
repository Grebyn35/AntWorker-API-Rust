//! The SQL of `verification_codes` (ARCH-10): a code is one user's for one purpose, found by both
//! (DATA-16), and expires by the database's clock (DATA-03). A row is never edited (DATA-19).

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

/// Inserts `user_id`'s code for `purpose` whose HMAC is `hash`, lasting `minutes` from now, in
/// place of the one before it, and returns its ID; or `None` when another unit of work issued one
/// meanwhile, which the unique constraint `verification_codes_user_id_purpose_key` refuses
/// without aborting the unit of work (AUTH-030, ERR-05).
pub(in crate::modules::auth) async fn replace(
    uow: &mut UnitOfWork,
    user_id: UserId,
    purpose: Purpose,
    hash: &[u8],
    minutes: i32,
) -> Result<Option<uuid::Uuid>, Internal> {
    sqlx::query!(
        "DELETE FROM verification_codes
        WHERE user_id = $1 AND purpose = $2::text::verification_purpose",
        user_id.uuid(),
        purpose.name(),
    )
    .execute(uow.executor())
    .await?;
    Ok(sqlx::query_scalar!(
        "INSERT INTO verification_codes (user_id, purpose, code_hash, expires_at)
        VALUES ($1, $2::text::verification_purpose, $3, now() + make_interval(mins => $4))
        ON CONFLICT ON CONSTRAINT verification_codes_user_id_purpose_key DO NOTHING
        RETURNING id",
        user_id.uuid(),
        purpose.name(),
        hash,
        minutes,
    )
    .fetch_optional(uow.executor())
    .await?)
}

/// `user_id`'s code for `purpose`, if any.
pub(in crate::modules::auth) async fn find(
    uow: &mut UnitOfWork,
    user_id: UserId,
    purpose: Purpose,
) -> Result<Option<StoredCode>, Internal> {
    let row = sqlx::query!(
        r#"SELECT id, code_hash, expires_at <= now() AS "expired!" FROM verification_codes
        WHERE user_id = $1 AND purpose = $2::text::verification_purpose"#,
        user_id.uuid(),
        purpose.name(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| StoredCode {
        id: row.id,
        hash: row.code_hash,
        expired: row.expired,
    }))
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
