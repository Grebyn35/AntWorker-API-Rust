//! The SQL of `totp_factors`, `totp_setups`, `recovery_codes` and `sign_in_challenges` (ARCH-10):
//! each row is one user's, found by them (DATA-16), or a challenge by its token's hash, and every
//! expiry comes from the database's clock (DATA-03). An app's secret is stored as the key ring
//! sealed it, with its version (SEC-02).

use platform::{
    db::UnitOfWork,
    encryption::Sealed,
    errors::Internal,
    types::{Timestamp, UserId},
};

use crate::modules::auth::model::{DeviceId, DeviceName};

/// A user's authenticator app, as a check reads it.
pub(in crate::modules::auth) struct StoredApp {
    pub(in crate::modules::auth) id: uuid::Uuid,
    /// Its secret, as the key ring sealed it.
    pub(in crate::modules::auth) secret: Sealed,
    /// The last step whose code it accepted (AUTH-057).
    pub(in crate::modules::auth) last_used_step: i64,
    /// When its setup was confirmed.
    pub(in crate::modules::auth) created_at: Timestamp,
}

/// A user's setup of an app that lasts, as its confirmation reads it.
pub(in crate::modules::auth) struct StoredSetup {
    pub(in crate::modules::auth) id: uuid::Uuid,
    /// Its secret, as the key ring sealed it.
    pub(in crate::modules::auth) secret: Sealed,
}

/// A sign-in's challenge, as its completion reads it (AUTH-059).
pub(in crate::modules::auth) struct StoredChallenge {
    pub(in crate::modules::auth) id: uuid::Uuid,
    pub(in crate::modules::auth) user_id: UserId,
    pub(in crate::modules::auth) device_id: Option<DeviceId>,
    pub(in crate::modules::auth) device_name: Option<DeviceName>,
    /// The provider of the sign-in, or `None` for a password (AUTH-073).
    pub(in crate::modules::auth) provider: Option<String>,
    /// The hash of the device token that the sign-in brought, when it was valid for the email.
    pub(in crate::modules::auth) device_token_hash: Option<[u8; 32]>,
    /// Whether its 5 minutes have passed, by the database's clock.
    pub(in crate::modules::auth) expired: bool,
}

/// A new row ID, from the database, as its tables' defaults draw one: a secret's ciphertext is
/// bound to the row that owns it, which exists before the secret is written (SEC-02).
pub(in crate::modules::auth) async fn new_id(uow: &mut UnitOfWork) -> Result<uuid::Uuid, Internal> {
    Ok(sqlx::query_scalar!(r#"SELECT uuidv7() AS "id!""#)
        .fetch_one(uow.executor())
        .await?)
}

/// The seconds since the Unix epoch, by the database's clock, at which a check of an app's code
/// runs (AUTH-057, DATA-03).
pub(in crate::modules::auth) async fn now(uow: &mut UnitOfWork) -> Result<i64, Internal> {
    Ok(
        sqlx::query_scalar!(r#"SELECT floor(extract(epoch FROM now()))::int8 AS "now!""#)
            .fetch_one(uow.executor())
            .await?,
    )
}

/// `user_id`'s app, without a lock, for reading their second factors (AUTH-054).
pub(in crate::modules::auth) async fn app(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Option<StoredApp>, Internal> {
    let row = sqlx::query!(
        "SELECT id, key_version, secret, last_used_step, created_at FROM totp_factors
        WHERE user_id = $1",
        user_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| StoredApp {
        id: row.id,
        secret: Sealed {
            version: row.key_version,
            bytes: row.secret,
        },
        last_used_step: row.last_used_step,
        created_at: Timestamp::from_database(row.created_at),
    }))
}

/// `user_id`'s app, locked `FOR UPDATE` until the unit of work ends, so two checks of its codes
/// take turns, and the second reads the step the first accepted (AUTH-057, DATA-20).
pub(in crate::modules::auth) async fn app_for_update(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Option<StoredApp>, Internal> {
    let row = sqlx::query!(
        "SELECT id, key_version, secret, last_used_step, created_at FROM totp_factors
        WHERE user_id = $1
        FOR UPDATE",
        user_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| StoredApp {
        id: row.id,
        secret: Sealed {
            version: row.key_version,
            bytes: row.secret,
        },
        last_used_step: row.last_used_step,
        created_at: Timestamp::from_database(row.created_at),
    }))
}

/// Whether `user_id` has an app, which a sign-in reads once their password matched (AUTH-058), and
/// the verifier as it authenticates each request while the deployment requires one (AUTH-070).
pub(in crate::modules::auth) async fn has_app(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<bool, Internal> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM totp_factors WHERE user_id = $1) AS "exists!""#,
        user_id.uuid(),
    )
    .fetch_one(uow.executor())
    .await?)
}

/// Records that `user_id`'s app `id` accepted the code of `step`, and stores its secret anew when
/// `resealed`, under the key ring's current version (AUTH-057, AUTH-066).
pub(in crate::modules::auth) async fn record_step(
    uow: &mut UnitOfWork,
    user_id: UserId,
    id: uuid::Uuid,
    step: i64,
    resealed: Option<&Sealed>,
) -> Result<(), Internal> {
    sqlx::query!(
        "UPDATE totp_factors
        SET last_used_step = $3,
            key_version = coalesce($4, key_version),
            secret = coalesce($5, secret)
        WHERE user_id = $1 AND id = $2",
        user_id.uuid(),
        id,
        step,
        resealed.map(|sealed| sealed.version),
        resealed.map(|sealed| sealed.bytes.as_slice()),
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Makes the app `id`, whose secret is `secret`, `user_id`'s, with `step` as its last used one,
/// in place of any before it (AUTH-056).
pub(in crate::modules::auth) async fn replace_app(
    uow: &mut UnitOfWork,
    user_id: UserId,
    id: uuid::Uuid,
    secret: &Sealed,
    step: i64,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM totp_factors WHERE user_id = $1",
        user_id.uuid()
    )
    .execute(uow.executor())
    .await?;
    sqlx::query!(
        "INSERT INTO totp_factors (id, user_id, key_version, secret, last_used_step)
        VALUES ($2, $1, $3, $4, $5)",
        user_id.uuid(),
        id,
        secret.version,
        secret.bytes,
        step,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Deletes `user_id`'s app, and gives its ID, or `None` without one (AUTH-064).
pub(in crate::modules::auth) async fn delete_app(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Option<uuid::Uuid>, Internal> {
    Ok(sqlx::query_scalar!(
        "DELETE FROM totp_factors WHERE user_id = $1 RETURNING id",
        user_id.uuid()
    )
    .fetch_optional(uow.executor())
    .await?)
}

/// Inserts `user_id`'s setup `id`, whose secret is `secret`, lasting `minutes` from now, in place
/// of the one before it, and returns when it expires (AUTH-055).
pub(in crate::modules::auth) async fn replace_setup(
    uow: &mut UnitOfWork,
    user_id: UserId,
    id: uuid::Uuid,
    secret: &Sealed,
    minutes: i32,
) -> Result<Timestamp, Internal> {
    sqlx::query!("DELETE FROM totp_setups WHERE user_id = $1", user_id.uuid())
        .execute(uow.executor())
        .await?;
    let expires_at = sqlx::query_scalar!(
        "INSERT INTO totp_setups (id, user_id, key_version, secret, expires_at)
        VALUES ($2, $1, $3, $4, now() + make_interval(mins => $5))
        RETURNING expires_at",
        user_id.uuid(),
        id,
        secret.version,
        secret.bytes,
        minutes,
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(Timestamp::from_database(expires_at))
}

/// `user_id`'s setup, while it lasts (AUTH-056).
pub(in crate::modules::auth) async fn lasting_setup(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Option<StoredSetup>, Internal> {
    let row = sqlx::query!(
        "SELECT id, key_version, secret FROM totp_setups
        WHERE user_id = $1 AND expires_at > now()",
        user_id.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| StoredSetup {
        id: row.id,
        secret: Sealed {
            version: row.key_version,
            bytes: row.secret,
        },
    }))
}

/// Deletes `user_id`'s setup `id`, which its confirmation ends, and says whether it did: a row
/// only one unit of work deletes, so of two confirmations, one alone confirms it (AUTH-056).
pub(in crate::modules::auth) async fn claim_setup(
    uow: &mut UnitOfWork,
    user_id: UserId,
    id: uuid::Uuid,
) -> Result<bool, Internal> {
    let deleted = sqlx::query!(
        "DELETE FROM totp_setups WHERE user_id = $1 AND id = $2",
        user_id.uuid(),
        id,
    )
    .execute(uow.executor())
    .await?
    .rows_affected();
    Ok(deleted == 1)
}

/// Makes `hashes` the SHA-256 hashes of `user_id`'s recovery codes, in place of those before them
/// (AUTH-056, AUTH-063).
pub(in crate::modules::auth) async fn replace_recovery_codes(
    uow: &mut UnitOfWork,
    user_id: UserId,
    hashes: &[Vec<u8>],
) -> Result<(), Internal> {
    delete_recovery_codes(uow, user_id).await?;
    sqlx::query!(
        "INSERT INTO recovery_codes (user_id, code_hash)
        SELECT $1, hash FROM unnest($2::bytea[]) AS codes (hash)",
        user_id.uuid(),
        hashes,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// How many recovery codes `user_id` holds (AUTH-054, AUTH-068).
pub(in crate::modules::auth) async fn count_recovery_codes(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<i64, Internal> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM recovery_codes WHERE user_id = $1"#,
        user_id.uuid(),
    )
    .fetch_one(uow.executor())
    .await?)
}

/// Deletes `user_id`'s recovery code whose hash is `hash`, and gives its ID, or `None` when the
/// user holds no such code: a row only one unit of work deletes, so a code works once
/// (AUTH-062).
pub(in crate::modules::auth) async fn claim_recovery_code(
    uow: &mut UnitOfWork,
    user_id: UserId,
    hash: &[u8],
) -> Result<Option<uuid::Uuid>, Internal> {
    Ok(sqlx::query_scalar!(
        "DELETE FROM recovery_codes WHERE user_id = $1 AND code_hash = $2 RETURNING id",
        user_id.uuid(),
        hash,
    )
    .fetch_optional(uow.executor())
    .await?)
}

/// Deletes every recovery code of `user_id` (AUTH-064).
pub(in crate::modules::auth) async fn delete_recovery_codes(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM recovery_codes WHERE user_id = $1",
        user_id.uuid()
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Inserts a challenge of `user_id`'s whose token hashes to `token`, on `device`, with the hash of
/// the valid device token that the sign-in brought, lasting `minutes` from now, and returns when
/// it expires (AUTH-058).
pub(in crate::modules::auth) async fn insert_challenge(
    uow: &mut UnitOfWork,
    user_id: UserId,
    token: &[u8],
    device: (Option<&DeviceId>, Option<&DeviceName>, Option<&str>),
    device_token: Option<&[u8; 32]>,
    minutes: i32,
) -> Result<Timestamp, Internal> {
    let expires_at = sqlx::query_scalar!(
        "INSERT INTO sign_in_challenges (
            user_id, token_hash, device_id, device_name, device_token_hash, expires_at, provider
        ) VALUES ($1, $2, $3, $4, $5, now() + make_interval(mins => $6), $7)
        RETURNING expires_at",
        user_id.uuid(),
        token,
        device.0.map(DeviceId::as_str),
        device.1.map(DeviceName::as_str),
        device_token.map(<[u8; 32]>::as_slice),
        minutes,
        device.2,
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(Timestamp::from_database(expires_at))
}

/// The challenge whose token hashes to `token`, found through its unique index (DATA-16).
pub(in crate::modules::auth) async fn challenge(
    uow: &mut UnitOfWork,
    token: &[u8],
) -> Result<Option<StoredChallenge>, Internal> {
    let row = sqlx::query!(
        r#"SELECT id, user_id, device_id, device_name, device_token_hash, provider,
            expires_at <= now() AS "expired!"
        FROM sign_in_challenges WHERE token_hash = $1"#,
        token,
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| {
        let device_token_hash = row
            .device_token_hash
            .map(|hash| {
                <[u8; 32]>::try_from(hash.as_slice())
                    .map_err(|_| Internal::new("a challenge's device token hash isn't 32 bytes"))
            })
            .transpose()?;
        Ok(StoredChallenge {
            id: row.id,
            user_id: UserId::from_uuid(row.user_id),
            device_id: row.device_id.map(DeviceId::from_stored),
            device_name: row.device_name.map(DeviceName::from_stored),
            provider: row.provider,
            device_token_hash,
            expired: row.expired,
        })
    })
    .transpose()
}

/// Deletes `user_id`'s challenge `id`, which its completion ends, while it lasts, and says whether
/// it did: a row only one unit of work deletes, so a challenge completes one sign-in, and none
/// that expired while its completion waited for its turn (AUTH-059, AUTH-060).
pub(in crate::modules::auth) async fn claim_challenge(
    uow: &mut UnitOfWork,
    user_id: UserId,
    id: uuid::Uuid,
) -> Result<bool, Internal> {
    let deleted = sqlx::query!(
        "DELETE FROM sign_in_challenges WHERE user_id = $1 AND id = $2 AND expires_at > now()",
        user_id.uuid(),
        id,
    )
    .execute(uow.executor())
    .await?
    .rows_affected();
    Ok(deleted == 1)
}

/// Deletes every challenge of `user_id`, so none issued before the unit of work completes a
/// sign-in (AUTH-069).
pub(in crate::modules::auth) async fn delete_challenges(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM sign_in_challenges WHERE user_id = $1",
        user_id.uuid()
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Deletes every challenge of `user_id` that a sign-in through `provider` issued, or one with the
/// password for `None`, so none that the removed identity's sign-in issued completes (AUTH-069,
/// AUTH-088).
pub(in crate::modules::auth) async fn delete_challenges_through(
    uow: &mut UnitOfWork,
    user_id: UserId,
    provider: Option<&str>,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM sign_in_challenges
        WHERE user_id = $1 AND provider IS NOT DISTINCT FROM $2",
        user_id.uuid(),
        provider,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Deletes `user_id`'s setup, whose account the unit of work deletes (ACCOUNTS-013).
pub(in crate::modules::auth) async fn delete_setup(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), Internal> {
    sqlx::query!("DELETE FROM totp_setups WHERE user_id = $1", user_id.uuid())
        .execute(uow.executor())
        .await?;
    Ok(())
}

/// The versions of the key ring under which an app's secret, or a lasting setup's, is stored, of
/// every user's rows: the one read that spans every user outside a job, which startup makes, and
/// which reads the versions alone (AUTH-066, DATA-16).
pub(in crate::modules::auth) async fn stored_key_versions(
    uow: &mut UnitOfWork,
) -> Result<Vec<i16>, Internal> {
    Ok(sqlx::query_scalar!(
        r#"SELECT key_version AS "key_version!" FROM totp_factors
        UNION
        SELECT key_version FROM totp_setups WHERE expires_at > now()"#,
    )
    .fetch_all(uow.executor())
    .await?)
}
