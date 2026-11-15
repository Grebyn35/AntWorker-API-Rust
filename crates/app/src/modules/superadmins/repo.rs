//! The SQL of `superadmins` (ARCH-10). Every query reads or writes the row of one user: the
//! caller, or the account that the operator's subcommand names by its address (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{Timestamp, UserId},
};

use super::model::Exported;

/// Whether `user` is a superadmin.
pub(super) async fn is_one(uow: &mut UnitOfWork, user: UserId) -> Result<bool, Internal> {
    let found = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM superadmins WHERE user_id = $1) AS "found!""#,
        user.uuid(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(found)
}

/// Makes `user` a superadmin, and returns whether it did: a user who is one already has a row,
/// which the unique constraint keeps the only one (ERR-05).
pub(super) async fn insert(uow: &mut UnitOfWork, user: UserId) -> Result<bool, Internal> {
    let inserted = sqlx::query!(
        "INSERT INTO superadmins (user_id) VALUES ($1) ON CONFLICT (user_id) DO NOTHING",
        user.uuid(),
    )
    .execute(uow.executor())
    .await?;
    Ok(inserted.rows_affected() == 1)
}

/// Ends `user`'s being a superadmin, and returns whether they were one.
pub(super) async fn delete(uow: &mut UnitOfWork, user: UserId) -> Result<bool, Internal> {
    let deleted = sqlx::query!("DELETE FROM superadmins WHERE user_id = $1", user.uuid())
        .execute(uow.executor())
        .await?;
    Ok(deleted.rows_affected() == 1)
}

/// The row of `user`, if they are a superadmin, as their export holds it (SUPERADMINS-007).
pub(super) async fn exported(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Option<Exported>, Internal> {
    let created_at = sqlx::query_scalar!(
        "SELECT created_at FROM superadmins WHERE user_id = $1",
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(created_at.map(|created_at| Exported {
        created_at: Timestamp::from_database(created_at),
    }))
}
