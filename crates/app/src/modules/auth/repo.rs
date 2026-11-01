//! The SQL of `identities` and `sessions` (ARCH-10). Every expiry comes from the database's clock
//! (DATA-03), and a session is found by a token's hash through its unique index (DATA-22).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{SessionId, Timestamp, UserId},
};

use super::model::{DeviceId, Lifetimes, Session};

/// A user's password identity.
pub(super) struct PasswordIdentity {
    pub(super) id: uuid::Uuid,
    pub(super) hash: String,
}

/// A session found by one of its token hashes.
pub(super) struct Found {
    pub(super) id: SessionId,
    pub(super) user_id: UserId,
    pub(super) expired: bool,
}

/// Seconds, as Postgres's `make_interval` takes them.
fn seconds(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64()
}

pub(super) async fn insert_password_identity(
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

pub(super) async fn password_identity(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Option<PasswordIdentity>, Internal> {
    let row = sqlx::query!(
        "SELECT id, password_hash FROM identities WHERE user_id = $1 AND method = 'password'",
        user_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.and_then(|row| {
        row.password_hash
            .map(|hash| PasswordIdentity { id: row.id, hash })
    }))
}

pub(super) async fn update_password_hash(
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

/// Inserts a session for `user_id`, whose tokens hash to `access` and `refresh`. No expiry passes
/// the absolute lifetime (AUTH-007).
pub(super) async fn insert_session(
    uow: &mut UnitOfWork,
    user_id: UserId,
    device_id: Option<&DeviceId>,
    access: &[u8],
    refresh: &[u8],
    lifetimes: Lifetimes,
) -> Result<Session, Internal> {
    let row = sqlx::query!(
        "INSERT INTO sessions (
            user_id, device_id, access_token_hash, refresh_token_hash, access_expires_at,
            refresh_expires_at, absolute_expires_at
        ) VALUES (
            $1, $2, $3, $4,
            now() + make_interval(secs => least($5::float8, $7::float8)),
            now() + make_interval(secs => least($6::float8, $7::float8)),
            now() + make_interval(secs => $7::float8)
        )
        RETURNING id, device_id, created_at, access_expires_at, refresh_expires_at,
            absolute_expires_at",
        user_id.uuid(),
        device_id.map(DeviceId::as_str),
        access,
        refresh,
        seconds(lifetimes.access),
        seconds(lifetimes.refresh),
        seconds(lifetimes.absolute),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(Session {
        id: SessionId::from_uuid(row.id),
        device_id: row.device_id.map(DeviceId::from_stored),
        created_at: Timestamp::from_database(row.created_at),
        access_token_expires_at: Timestamp::from_database(row.access_expires_at),
        refresh_token_expires_at: Timestamp::from_database(row.refresh_expires_at),
        expires_at: Timestamp::from_database(row.absolute_expires_at),
    })
}

/// The session `user_id` has on `device_id`, if any (AUTH-008).
pub(super) async fn session_on_device(
    uow: &mut UnitOfWork,
    user_id: UserId,
    device_id: &DeviceId,
) -> Result<Option<SessionId>, Internal> {
    let id = sqlx::query_scalar!(
        "SELECT id FROM sessions WHERE user_id = $1 AND device_id = $2",
        user_id.uuid(),
        device_id.as_str(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(id.map(SessionId::from_uuid))
}

pub(super) async fn delete_session(
    uow: &mut UnitOfWork,
    user_id: UserId,
    session_id: SessionId,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM sessions WHERE id = $2 AND user_id = $1",
        user_id.uuid(),
        session_id.uuid(),
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// The session whose access token hashes to `hash`, and whether that token has expired.
pub(super) async fn by_access_token(
    uow: &mut UnitOfWork,
    hash: &[u8],
) -> Result<Option<Found>, Internal> {
    let row = sqlx::query!(
        r#"SELECT id, user_id, access_expires_at <= now() AS "expired!"
        FROM sessions WHERE access_token_hash = $1"#,
        hash,
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Found {
        id: SessionId::from_uuid(row.id),
        user_id: UserId::from_uuid(row.user_id),
        expired: row.expired,
    }))
}

/// The session whose current refresh token hashes to `hash`, and whether that token, or the
/// session, has expired (AUTH-011).
pub(super) async fn by_refresh_token(
    uow: &mut UnitOfWork,
    hash: &[u8],
) -> Result<Option<Found>, Internal> {
    let row = sqlx::query!(
        r#"SELECT id, user_id,
            refresh_expires_at <= now() OR absolute_expires_at <= now() AS "expired!"
        FROM sessions WHERE refresh_token_hash = $1"#,
        hash,
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Found {
        id: SessionId::from_uuid(row.id),
        user_id: UserId::from_uuid(row.user_id),
        expired: row.expired,
    }))
}

/// The session whose previous refresh token hashes to `hash`: a refresh token presented again
/// after its rotation (AUTH-012).
pub(super) async fn by_previous_refresh_token(
    uow: &mut UnitOfWork,
    hash: &[u8],
) -> Result<Option<Found>, Internal> {
    let row = sqlx::query!(
        "SELECT id, user_id FROM sessions WHERE previous_refresh_token_hash = $1",
        hash,
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Found {
        id: SessionId::from_uuid(row.id),
        user_id: UserId::from_uuid(row.user_id),
        expired: false,
    }))
}

/// Gives the session `id` new tokens, if `presented` is still its refresh token's hash, so two
/// concurrent refreshes can't both succeed (AUTH-010). The refresh expiry slides, never past the
/// absolute lifetime.
pub(super) async fn rotate(
    uow: &mut UnitOfWork,
    found: &Found,
    presented: &[u8],
    access: &[u8],
    refresh: &[u8],
    lifetimes: Lifetimes,
) -> Result<Option<Session>, Internal> {
    let row = sqlx::query!(
        "UPDATE sessions SET
            access_token_hash = $4,
            refresh_token_hash = $5,
            previous_refresh_token_hash = $3,
            access_expires_at = least(now() + make_interval(secs => $6::float8), absolute_expires_at),
            refresh_expires_at = least(now() + make_interval(secs => $7::float8), absolute_expires_at)
        WHERE id = $2 AND user_id = $1 AND refresh_token_hash = $3
        RETURNING id, device_id, created_at, access_expires_at, refresh_expires_at,
            absolute_expires_at",
        found.user_id.uuid(),
        found.id.uuid(),
        presented,
        access,
        refresh,
        seconds(lifetimes.access),
        seconds(lifetimes.refresh),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Session {
        id: SessionId::from_uuid(row.id),
        device_id: row.device_id.map(DeviceId::from_stored),
        created_at: Timestamp::from_database(row.created_at),
        access_token_expires_at: Timestamp::from_database(row.access_expires_at),
        refresh_token_expires_at: Timestamp::from_database(row.refresh_expires_at),
        expires_at: Timestamp::from_database(row.absolute_expires_at),
    }))
}

/// The session `session_id` of `user_id` (AUTH-016).
pub(super) async fn session(
    uow: &mut UnitOfWork,
    user_id: UserId,
    session_id: SessionId,
) -> Result<Option<Session>, Internal> {
    let row = sqlx::query!(
        "SELECT id, device_id, created_at, access_expires_at, refresh_expires_at,
            absolute_expires_at
        FROM sessions WHERE id = $2 AND user_id = $1",
        user_id.uuid(),
        session_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Session {
        id: SessionId::from_uuid(row.id),
        device_id: row.device_id.map(DeviceId::from_stored),
        created_at: Timestamp::from_database(row.created_at),
        access_token_expires_at: Timestamp::from_database(row.access_expires_at),
        refresh_token_expires_at: Timestamp::from_database(row.refresh_expires_at),
        expires_at: Timestamp::from_database(row.absolute_expires_at),
    }))
}
