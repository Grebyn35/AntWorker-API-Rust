//! The SQL of `identities` and `provider_identities` (ARCH-10): a user's ways of signing in, of
//! which a password identity holds its password's hash (AUTH-014), and a provider identity the
//! provider's ID for the user (AUTH-073).

use platform::{db::UnitOfWork, errors::Internal, types::UserId};

/// A user's password identity.
pub(in crate::modules::auth) struct PasswordIdentity {
    pub(in crate::modules::auth) id: uuid::Uuid,
    pub(in crate::modules::auth) hash: String,
}

pub(in crate::modules::auth) async fn insert_password_identity(
    uow: &mut UnitOfWork,
    user_id: UserId,
    hash: &str,
) -> Result<(), Internal> {
    sqlx::query!(
        "INSERT INTO identities (user_id, method, password_hash) VALUES ($1, 'password', $2)",
        user_id.uuid(),
        hash,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// The password identity of `user_id`, locked until the unit of work ends, so the sign-ins of
/// one user take turns (DATA-20).
pub(in crate::modules::auth) async fn password_identity_for_update(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Option<PasswordIdentity>, Internal> {
    let row = sqlx::query!(
        "SELECT id, password_hash FROM identities WHERE user_id = $1 AND method = 'password'
        FOR UPDATE",
        user_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.and_then(|row| {
        row.password_hash
            .map(|hash| PasswordIdentity { id: row.id, hash })
    }))
}

/// The password hash of `user_id`'s password identity, if they have one, read without a lock,
/// for a check of a signed-in user's password (AUTH-021).
pub(in crate::modules::auth) async fn password_hash(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Option<String>, Internal> {
    Ok(sqlx::query_scalar!(
        "SELECT password_hash FROM identities WHERE user_id = $1 AND method = 'password'",
        user_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?
    .flatten())
}

pub(in crate::modules::auth) async fn update_password_hash(
    uow: &mut UnitOfWork,
    user_id: UserId,
    identity_id: uuid::Uuid,
    hash: &str,
) -> Result<(), Internal> {
    sqlx::query!(
        "UPDATE identities SET password_hash = $3 WHERE id = $2 AND user_id = $1",
        user_id.uuid(),
        identity_id,
        hash,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Sets the hash of `user_id`'s password identity to `hash`, inserting the identity when they have
/// none, and returns its ID. Setting it waits for the lock of a sign-in that holds the identity,
/// so that sign-in's session exists before the caller ends the user's sessions (AUTH-037,
/// AUTH-040, DATA-20).
pub(in crate::modules::auth) async fn set_password(
    uow: &mut UnitOfWork,
    user_id: UserId,
    hash: &str,
) -> Result<uuid::Uuid, Internal> {
    Ok(sqlx::query_scalar!(
        "INSERT INTO identities (user_id, method, password_hash) VALUES ($1, 'password', $2)
        ON CONFLICT ON CONSTRAINT identities_user_id_method_key
        DO UPDATE SET password_hash = EXCLUDED.password_hash
        RETURNING id",
        user_id.uuid(),
        hash,
    )
    .fetch_one(uow.executor())
    .await?)
}

/// Deletes every identity of `user_id`, their password's, then their providers', whose account
/// the unit of work deletes (ACCOUNTS-013).
pub(in crate::modules::auth) async fn delete_identities(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), Internal> {
    sqlx::query!("DELETE FROM identities WHERE user_id = $1", user_id.uuid())
        .execute(uow.executor())
        .await?;
    sqlx::query!(
        "DELETE FROM provider_identities WHERE user_id = $1",
        user_id.uuid()
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// The user whose provider identity at `provider` holds `subject`, found through its unique
/// index, before the request knows any user, as a sign-in with a password finds its account by its
/// address (AUTH-073).
pub(in crate::modules::auth) async fn user_of_subject(
    uow: &mut UnitOfWork,
    provider: &str,
    subject: &str,
) -> Result<Option<UserId>, Internal> {
    Ok(sqlx::query_scalar!(
        "SELECT user_id FROM provider_identities WHERE provider = $1 AND subject = $2",
        provider,
        subject,
    )
    .fetch_optional(uow.executor())
    .await?
    .map(UserId::from_uuid))
}

/// Inserts `user_id`'s provider identity at `provider`, which holds `subject`, for a registration
/// through the provider (AUTH-074).
pub(in crate::modules::auth) async fn insert_provider_identity(
    uow: &mut UnitOfWork,
    user_id: UserId,
    provider: &str,
    subject: &str,
) -> Result<(), Internal> {
    sqlx::query!(
        "INSERT INTO provider_identities (user_id, provider, subject) VALUES ($1, $2, $3)",
        user_id.uuid(),
        provider,
        subject,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// The subject of `user_id`'s provider identity at `provider`, if they have one (AUTH-080).
pub(in crate::modules::auth) async fn provider_subject(
    uow: &mut UnitOfWork,
    user_id: UserId,
    provider: &str,
) -> Result<Option<String>, Internal> {
    Ok(sqlx::query_scalar!(
        "SELECT subject FROM provider_identities WHERE user_id = $1 AND provider = $2",
        user_id.uuid(),
        provider,
    )
    .fetch_optional(uow.executor())
    .await?)
}
