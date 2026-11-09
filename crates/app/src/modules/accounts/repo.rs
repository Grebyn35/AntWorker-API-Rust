//! The SQL of `users` (ARCH-10).

use platform::{
    db::{self, UnitOfWork},
    errors::Internal,
    lists::Search,
    personal_data::ErasureError,
    types::{EmailAddress, UserId, Version},
};

use super::model::{Current, DisplayName, Profile, PublicProfile};

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

/// The ID of the user with `email`, through its unique index (DATA-16).
pub(super) async fn find_by_email(
    uow: &mut UnitOfWork,
    email: &EmailAddress,
) -> Result<Option<UserId>, Internal> {
    let id = sqlx::query_scalar!("SELECT id FROM users WHERE email = $1", email.as_str())
        .fetch_optional(uow.executor())
        .await?;
    Ok(id.map(UserId::from_uuid))
}

/// The record of the user `id`, with its version, filtered by the acting user's own ID
/// (DATA-16).
pub(super) async fn profile(uow: &mut UnitOfWork, id: UserId) -> Result<Option<Current>, Internal> {
    let row = sqlx::query!(
        "SELECT id, email, name, email_verified, version FROM users WHERE id = $1",
        id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Current {
        profile: Profile {
            id: UserId::from_uuid(row.id),
            email: EmailAddress::from_stored(row.email),
            name: DisplayName::from_stored(row.name),
            email_verified: row.email_verified,
            // part: files
            avatar_url: None,
            // end part: files
        },
        version: Version::from_database(row.version),
    }))
}

/// Locks the row of the user `id` until the unit of work ends, `FOR UPDATE`, which the insert of
/// a membership of theirs waits for, since its foreign key reads the row `FOR KEY SHARE` (DATA-20,
/// the accounts spec's "Renaming").
pub(super) async fn lock(uow: &mut UnitOfWork, id: UserId) -> Result<(), Internal> {
    sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", id.uuid())
        .fetch_one(uow.executor())
        .await?;
    Ok(())
}

/// Locks the row of the user `id`, then deletes it, which deletes, or sets to null, through the
/// foreign keys, every row that still names them, as the last step of deleting their account,
/// whose unit of work bounds its waits for a lock. `Interrupted` is a wait that ran out, or a
/// membership that a request of theirs added meanwhile, whose foreign key refuses the deletion
/// (ACCOUNTS-013, ACCOUNTS-016).
pub(super) async fn delete(uow: &mut UnitOfWork, id: UserId) -> Result<(), ErasureError> {
    sqlx::query!("SELECT id FROM users WHERE id = $1 FOR UPDATE", id.uuid())
        .fetch_one(uow.executor())
        .await
        .map_err(interrupted)?;
    sqlx::query!("DELETE FROM users WHERE id = $1", id.uuid())
        .execute(uow.executor())
        .await
        .map_err(interrupted)?;
    Ok(())
}

/// A failure of the last step of a deletion: `Interrupted` for a wait for a lock that ran out, or
/// a foreign key that refused it, and an internal error otherwise (ERR-04).
fn interrupted(error: sqlx::Error) -> ErasureError {
    let refused = error
        .as_database_error()
        .is_some_and(|error| error.is_foreign_key_violation());
    if refused || db::is_lock_timeout(&error) {
        ErasureError::Interrupted
    } else {
        ErasureError::Internal(error.into())
    }
}

/// The versions that `versions` names, as the database takes them: none for any.
fn values(versions: Option<&[Version]>) -> Option<Vec<i64>> {
    versions.map(|versions| versions.iter().map(|version| version.value()).collect())
}

/// Renames the user `id`, when its version is one of `versions`, or whatever it is when there
/// are none, and returns whether it did (API-13, DATA-18).
pub(super) async fn rename(
    uow: &mut UnitOfWork,
    id: UserId,
    name: &DisplayName,
    versions: Option<&[Version]>,
) -> Result<bool, Internal> {
    let versions = values(versions);
    let renamed = sqlx::query!(
        "UPDATE users SET name = $2 WHERE id = $1 AND ($3::bigint[] IS NULL OR version = ANY($3))",
        id.uuid(),
        name.as_str(),
        versions.as_deref(),
    )
    .execute(uow.executor())
    .await?;
    Ok(renamed.rows_affected() > 0)
}

// part: files
/// Moves the version of the user `id`, for a change of their avatar, when it is one of
/// `versions`, or whatever it is when there are none, and returns whether it did (API-13,
/// DATA-18, the accounts spec's "Avatars").
pub(super) async fn touch(
    uow: &mut UnitOfWork,
    id: UserId,
    versions: Option<&[Version]>,
) -> Result<bool, Internal> {
    // The row's trigger moves its version and its update time (DATA-02).
    let versions = values(versions);
    let touched = sqlx::query!(
        "UPDATE users SET version = version
        WHERE id = $1 AND ($2::bigint[] IS NULL OR version = ANY($2))",
        id.uuid(),
        versions.as_deref(),
    )
    .execute(uow.executor())
    .await?;
    Ok(touched.rows_affected() > 0)
}
// end part: files

/// The names and emails of `ids`, which came from a query filtered by tenant (DATA-16).
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
            // part: files
            avatar_url: None,
            // end part: files
        })
        .collect())
}

/// The IDs among `ids`, which came from a query filtered by tenant, of the users whose name or
/// email contains `search`, ignoring case in every script (DATA-16, PLATFORM-047).
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
