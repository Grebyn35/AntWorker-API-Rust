//! The SQL of `users` (ARCH-10).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{EmailAddress, UserId},
};

use super::model::{DisplayName, Profile};

/// Inserts a user, or `None` when another user has the email: the unique constraint
/// `users_email_key` refuses the row, without aborting the unit of work (ACCOUNTS-004, ERR-05).
pub(super) async fn insert(
    uow: &mut UnitOfWork,
    email: &EmailAddress,
    name: &DisplayName,
) -> Result<Option<UserId>, Internal> {
    let id = sqlx::query_scalar!(
        "INSERT INTO users (email, name) VALUES ($1, $2)
        ON CONFLICT ON CONSTRAINT users_email_key DO NOTHING
        RETURNING id",
        email.as_str(),
        name.as_str(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(id.map(UserId::from_uuid))
}

/// The ID of the user with `email`, through its unique index (DATA-22).
pub(super) async fn find_by_email(
    uow: &mut UnitOfWork,
    email: &EmailAddress,
) -> Result<Option<UserId>, Internal> {
    let id = sqlx::query_scalar!("SELECT id FROM users WHERE email = $1", email.as_str())
        .fetch_optional(uow.executor())
        .await?;
    Ok(id.map(UserId::from_uuid))
}

/// The record of the user `id`, filtered by the acting user's own ID (DATA-22).
pub(super) async fn profile(uow: &mut UnitOfWork, id: UserId) -> Result<Option<Profile>, Internal> {
    let row = sqlx::query!(
        "SELECT id, email, name, email_verified FROM users WHERE id = $1",
        id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Profile {
        id: UserId::from_uuid(row.id),
        email: EmailAddress::from_stored(row.email),
        name: DisplayName::from_stored(row.name),
        email_verified: row.email_verified,
    }))
}
