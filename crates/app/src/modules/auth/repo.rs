//! The SQL of `identities`, `sessions` and `device_tokens` (ARCH-10). Every expiry comes from the
//! database's clock (DATA-03), and a session or a device token is found by a token's hash through
//! its unique index (DATA-16).

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

/// The session of a row that holds a session's columns.
macro_rules! session {
    ($row:expr) => {{
        let row = $row;
        Session {
            id: SessionId::from_uuid(row.id),
            user_id: UserId::from_uuid(row.user_id),
            device_id: row.device_id.map(DeviceId::from_stored),
            created_at: Timestamp::from_database(row.created_at),
            access_token_expires_at: Timestamp::from_database(row.access_token_expires_at),
            refresh_token_expires_at: Timestamp::from_database(row.refresh_token_expires_at),
            expires_at: Timestamp::from_database(row.expires_at),
        }
    }};
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

/// The password identity of `user_id`, locked until the unit of work ends, so the sign-ins of
/// one user take turns (DATA-20).
pub(super) async fn password_identity_for_update(
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
            user_id, device_id, access_token_hash, refresh_token_hash, access_token_expires_at,
            refresh_token_expires_at, expires_at
        ) VALUES (
            $1, $2, $3, $4,
            now() + make_interval(secs => least($5::float8, $7::float8)),
            now() + make_interval(secs => least($6::float8, $7::float8)),
            now() + make_interval(secs => $7::float8)
        )
        RETURNING id, user_id, device_id, created_at, access_token_expires_at, refresh_token_expires_at,
            expires_at",
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
    Ok(session!(row))
}

/// Whether the device token that hashes to `token` is one of the email whose HMAC is `email`, and
/// hasn't expired (AUTH-019).
pub(super) async fn device_token_valid(
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
pub(super) async fn end_device_tokens(
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
pub(super) async fn insert_device_token(
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
        seconds(lifetime),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok((row.id, Timestamp::from_database(row.expires_at)))
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

/// The sessions among `sessions`, each with its user, that can still be refreshed: their
/// refresh token and their absolute lifetime haven't expired (AUTH-018).
pub(super) async fn lasting(
    uow: &mut UnitOfWork,
    sessions: &[(UserId, SessionId)],
) -> Result<Vec<SessionId>, Internal> {
    let users: Vec<uuid::Uuid> = sessions.iter().map(|(user, _)| user.uuid()).collect();
    let ids: Vec<uuid::Uuid> = sessions.iter().map(|(_, id)| id.uuid()).collect();
    let lasting = sqlx::query_scalar!(
        "SELECT sessions.id FROM sessions
        JOIN unnest($1::uuid[], $2::uuid[]) AS asked(user_id, id)
            ON sessions.user_id = asked.user_id AND sessions.id = asked.id
        WHERE sessions.refresh_token_expires_at > now() AND sessions.expires_at > now()",
        &users,
        &ids,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(lasting.into_iter().map(SessionId::from_uuid).collect())
}

/// The session whose access token hashes to `hash`, and whether that token has expired.
pub(super) async fn by_access_token(
    uow: &mut UnitOfWork,
    hash: &[u8],
) -> Result<Option<Found>, Internal> {
    let row = sqlx::query!(
        r#"SELECT id, user_id, access_token_expires_at <= now() AS "expired!"
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
            refresh_token_expires_at <= now() OR expires_at <= now() AS "expired!"
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
            access_token_expires_at = least(now() + make_interval(secs => $6::float8), expires_at),
            refresh_token_expires_at = least(now() + make_interval(secs => $7::float8), expires_at)
        WHERE id = $2 AND user_id = $1 AND refresh_token_hash = $3
        RETURNING id, user_id, device_id, created_at, access_token_expires_at, refresh_token_expires_at,
            expires_at",
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
    Ok(row.map(|row| session!(row)))
}

/// The session `session_id` of `user_id` (AUTH-016).
pub(super) async fn session(
    uow: &mut UnitOfWork,
    user_id: UserId,
    session_id: SessionId,
) -> Result<Option<Session>, Internal> {
    let row = sqlx::query!(
        "SELECT id, user_id, device_id, created_at, access_token_expires_at, refresh_token_expires_at,
            expires_at
        FROM sessions WHERE id = $2 AND user_id = $1",
        user_id.uuid(),
        session_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| session!(row)))
}
