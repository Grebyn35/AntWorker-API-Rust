//! The SQL of `users` (ARCH-10).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{EmailAddress, UserId},
};

use platform::lists::Search;

use super::model::{DisplayName, Profile, PublicProfile};

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

/// The names and emails of `ids`, which came from a query filtered by tenant (DATA-22).
pub(super) async fn profiles(
    uow: &mut UnitOfWork,
    ids: &[UserId],
) -> Result<Vec<PublicProfile>, Internal> {
    let ids: Vec<uuid::Uuid> = ids.iter().map(|id| id.uuid()).collect();
    let rows = sqlx::query!("SELECT id, email, name FROM users WHERE id = ANY($1)", &ids,)
        .fetch_all(uow.executor())
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| PublicProfile {
            id: UserId::from_uuid(row.id),
            email: EmailAddress::from_stored(row.email),
            name: DisplayName::from_stored(row.name),
        })
        .collect())
}

/// The IDs among `ids`, which came from a query filtered by tenant, of the users whose name or
/// email contains `search`, ignoring case in every script (DATA-22, PLATFORM-060).
pub(super) async fn matching(
    uow: &mut UnitOfWork,
    ids: &[UserId],
    search: &Search,
) -> Result<Vec<UserId>, Internal> {
    let ids: Vec<uuid::Uuid> = ids.iter().map(|id| id.uuid()).collect();
    let matched = sqlx::query_scalar!(
        r#"SELECT id FROM users
        WHERE id = ANY($1)
            AND (name COLLATE pg_unicode_fast ILIKE $2 OR email COLLATE pg_unicode_fast ILIKE $2)"#,
        &ids,
        search.pattern(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(matched.into_iter().map(UserId::from_uuid).collect())
}
