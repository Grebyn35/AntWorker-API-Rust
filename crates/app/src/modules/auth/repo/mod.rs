//! The SQL of `sessions` and `device_tokens` (ARCH-10), with that of `identities` in
//! `identities.rs`, that of `verification_codes` in `codes.rs`, and that of second factors and
//! sign-in challenges in `second_factors.rs`. Every expiry comes from the database's clock
//! (DATA-03), and a session or a device token is found by a token's hash through its unique index
//! (DATA-16). What a user's export holds of them is in `exported.rs`.

pub(super) mod codes;
pub(super) mod exported;
mod identities;
pub(super) mod second_factors;

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::{Page, Search},
    types::{SessionId, Timestamp, UserId},
};

use super::model::{DeviceId, DeviceName, Lifetimes, StoredSession};

pub(super) use identities::{
    add_password, delete_identities, delete_password_identity, delete_provider_identity,
    insert_password_identity, insert_provider_identity, password_hash, password_identity,
    password_identity_for_update, provider_identities, provider_subject, set_password,
    update_password_hash, user_of_subject,
};

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
        StoredSession {
            id: SessionId::from_uuid(row.id),
            user_id: UserId::from_uuid(row.user_id),
            device_id: row.device_id.map(DeviceId::from_stored),
            device_name: row.device_name.map(DeviceName::from_stored),
            provider: row.provider,
            created_at: Timestamp::from_database(row.created_at),
            access_token_expires_at: Timestamp::from_database(row.access_token_expires_at),
            refresh_token_expires_at: Timestamp::from_database(row.refresh_token_expires_at),
            expires_at: Timestamp::from_database(row.expires_at),
            authenticated_at: Timestamp::from_database(row.authenticated_at),
        }
    }};
}

/// Seconds, as Postgres's `make_interval` takes them.
fn seconds(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64()
}

/// Inserts a session for `user_id`, on the device `device`, whose tokens hash to `access` and
/// `refresh`. No expiry passes the absolute lifetime (AUTH-007).
pub(super) async fn insert_session(
    uow: &mut UnitOfWork,
    user_id: UserId,
    device: (Option<&DeviceId>, Option<&DeviceName>, Option<&str>),
    access: &[u8],
    refresh: &[u8],
    lifetimes: Lifetimes,
) -> Result<StoredSession, Internal> {
    let row = sqlx::query!(
        "INSERT INTO sessions (
            user_id, device_id, device_name, provider, access_token_hash, refresh_token_hash,
            access_token_expires_at, refresh_token_expires_at, expires_at
        ) VALUES (
            $1, $2, $8, $9, $3, $4,
            now() + make_interval(secs => least($5::float8, $7::float8)),
            now() + make_interval(secs => least($6::float8, $7::float8)),
            now() + make_interval(secs => $7::float8)
        )
        RETURNING id, user_id, device_id, device_name, created_at, access_token_expires_at,
            refresh_token_expires_at, expires_at, authenticated_at, provider",
        user_id.uuid(),
        device.0.map(DeviceId::as_str),
        access,
        refresh,
        seconds(lifetimes.access),
        seconds(lifetimes.refresh),
        seconds(lifetimes.absolute),
        device.1.map(DeviceName::as_str),
        device.2,
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

/// Deletes the session `session_id` of `user_id`, and returns whether it was there to delete
/// (AUTH-013, AUTH-044).
pub(super) async fn delete_session(
    uow: &mut UnitOfWork,
    user_id: UserId,
    session_id: SessionId,
) -> Result<bool, Internal> {
    let deleted = sqlx::query!(
        "DELETE FROM sessions WHERE id = $2 AND user_id = $1",
        user_id.uuid(),
        session_id.uuid(),
    )
    .execute(uow.executor())
    .await?
    .rows_affected();
    Ok(deleted > 0)
}

/// A page of `user_id`'s sessions that last, as [`lasting`] decides, the latest started first,
/// kept to those whose device's name contains `search`, ignoring case in every script, read
/// through the index on the user and the creation time (AUTH-043, DATA-17, PLATFORM-047).
pub(super) async fn page(
    uow: &mut UnitOfWork,
    user_id: UserId,
    search: Option<&Search>,
    page: &Page,
) -> Result<Vec<StoredSession>, Internal> {
    let rows = sqlx::query!(
        "SELECT id, user_id, device_id, device_name, created_at, access_token_expires_at,
            refresh_token_expires_at, expires_at, authenticated_at, provider
        FROM sessions
        WHERE user_id = $1
            AND refresh_token_expires_at > now() AND expires_at > now()
            AND ($2::text IS NULL OR device_name COLLATE pg_unicode_fast ILIKE $2)
            AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
        ORDER BY created_at DESC, id DESC
        LIMIT $5",
        user_id.uuid(),
        search.map(Search::pattern),
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows.into_iter().map(|row| session!(row)).collect())
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

// part: files
/// The user of the session `id`, if it lasts, as [`lasting`] decides, and whether `until`, a
/// signed URL's time, has passed, by the database's clock (AUTH-020, DATA-03). The session's ID
/// is one that a signed URL's signature proves the API gave (DATA-16).
pub(super) async fn lasting_session(
    uow: &mut UnitOfWork,
    id: SessionId,
    until: time::OffsetDateTime,
) -> Result<Option<(UserId, bool)>, Internal> {
    let row = sqlx::query!(
        r#"SELECT user_id, $2::timestamptz <= now() AS "passed!" FROM sessions
        WHERE id = $1 AND refresh_token_expires_at > now() AND expires_at > now()"#,
        id.uuid(),
        until,
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| (UserId::from_uuid(row.user_id), row.passed)))
}
// end part: files

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
) -> Result<Option<StoredSession>, Internal> {
    let row = sqlx::query!(
        "UPDATE sessions SET
            access_token_hash = $4,
            refresh_token_hash = $5,
            previous_refresh_token_hash = $3,
            access_token_expires_at = least(now() + make_interval(secs => $6::float8), expires_at),
            refresh_token_expires_at = least(now() + make_interval(secs => $7::float8), expires_at)
        WHERE id = $2 AND user_id = $1 AND refresh_token_hash = $3
        RETURNING id, user_id, device_id, device_name, created_at, access_token_expires_at,
            refresh_token_expires_at, expires_at, authenticated_at, provider",
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
) -> Result<Option<StoredSession>, Internal> {
    let row = sqlx::query!(
        "SELECT id, user_id, device_id, device_name, created_at, access_token_expires_at,
            refresh_token_expires_at, expires_at, authenticated_at, provider
        FROM sessions WHERE id = $2 AND user_id = $1",
        user_id.uuid(),
        session_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| session!(row)))
}

/// Sets the session `session_id` of `user_id` as re-authenticated now, by the database's clock,
/// and returns it, if it exists (AUTH-021).
pub(super) async fn reauthenticate(
    uow: &mut UnitOfWork,
    user_id: UserId,
    session_id: SessionId,
) -> Result<Option<StoredSession>, Internal> {
    let row = sqlx::query!(
        "UPDATE sessions SET authenticated_at = now() WHERE id = $2 AND user_id = $1
        RETURNING id, user_id, device_id, device_name, created_at, access_token_expires_at,
            refresh_token_expires_at, expires_at, authenticated_at, provider",
        user_id.uuid(),
        session_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| session!(row)))
}

/// Whether the session `session_id` of `user_id` proved who its user is within the last 5
/// minutes, by the database's clock, or `None` when the session is gone (AUTH-023, AUTH-044,
/// AUTH-045).
pub(super) async fn recently_authenticated(
    uow: &mut UnitOfWork,
    user_id: UserId,
    session_id: SessionId,
) -> Result<Option<bool>, Internal> {
    Ok(sqlx::query_scalar!(
        r#"SELECT authenticated_at > now() - interval '5 minutes' AS "recent!" FROM sessions
        WHERE id = $2 AND user_id = $1"#,
        user_id.uuid(),
        session_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?)
}

/// Deletes every session of `user_id`, whose account the unit of work deletes (ACCOUNTS-013), or
/// whose password it resets (AUTH-037), and returns their IDs.
pub(super) async fn delete_sessions(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Vec<SessionId>, Internal> {
    let ids = sqlx::query_scalar!(
        "DELETE FROM sessions WHERE user_id = $1 RETURNING id",
        user_id.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(ids.into_iter().map(SessionId::from_uuid).collect())
}

/// Deletes every session of `user_id` but `kept`, the one whose request changes their password or
/// ends their other sessions, and returns their IDs (AUTH-040, AUTH-045).
pub(super) async fn delete_other_sessions(
    uow: &mut UnitOfWork,
    user_id: UserId,
    kept: SessionId,
) -> Result<Vec<SessionId>, Internal> {
    let ids = sqlx::query_scalar!(
        "DELETE FROM sessions WHERE user_id = $1 AND id <> $2 RETURNING id",
        user_id.uuid(),
        kept.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(ids.into_iter().map(SessionId::from_uuid).collect())
}

/// Deletes every session of `user_id` but `kept` that signed in through `provider`, or with the
/// password for `None`, as its `provider` records, and returns their IDs: the sessions that an
/// identity started, whose removal `kept`'s request makes (AUTH-088).
pub(super) async fn delete_other_sessions_through(
    uow: &mut UnitOfWork,
    user_id: UserId,
    kept: SessionId,
    provider: Option<&str>,
) -> Result<Vec<SessionId>, Internal> {
    let ids = sqlx::query_scalar!(
        "DELETE FROM sessions
        WHERE user_id = $1 AND id <> $2 AND provider IS NOT DISTINCT FROM $3
        RETURNING id",
        user_id.uuid(),
        kept.uuid(),
        provider,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(ids.into_iter().map(SessionId::from_uuid).collect())
}

/// Deletes every device token of `user_id`, whose account the unit of work deletes
/// (ACCOUNTS-013).
pub(super) async fn delete_device_tokens(
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
