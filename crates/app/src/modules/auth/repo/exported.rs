//! The SQL of what a user's export holds of their identities, sessions, device tokens,
//! verification codes and second factors: never a password's, a token's or a code's hash
//! (SEC-03), nor an app's secret (SEC-02), and filtered by the user whose export the job builds
//! (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    personal_data::PAGE,
    types::{Timestamp, UserId},
};

use crate::modules::auth::model::{
    DeviceId, DeviceName, ExportedApp, ExportedChallenge, ExportedCode, ExportedDeviceToken,
    ExportedIdentity, ExportedRecoveryCode, ExportedSession, ExportedSetup, IdentityMethod,
    Purpose,
};

/// Every identity of `user`'s: one at most for each way to sign in.
pub(in crate::modules::auth) async fn identities(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<ExportedIdentity>, Internal> {
    let rows = sqlx::query!(
        r#"SELECT method::text AS "method!", created_at, updated_at FROM identities
        WHERE user_id = $1
        ORDER BY method"#,
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(ExportedIdentity {
                method: IdentityMethod::from_name(&row.method)?,
                created_at: Timestamp::from_database(row.created_at),
                updated_at: Timestamp::from_database(row.updated_at),
            })
        })
        .collect()
}

/// A page of `user`'s sessions, the latest first, after `after`, a session's creation time and ID.
pub(in crate::modules::auth) async fn sessions(
    uow: &mut UnitOfWork,
    user: UserId,
    after: Option<(time::OffsetDateTime, uuid::Uuid)>,
) -> Result<Vec<ExportedSession>, Internal> {
    let rows = sqlx::query!(
        "SELECT id, device_id, device_name, created_at, updated_at, authenticated_at,
            access_token_expires_at, refresh_token_expires_at, expires_at
        FROM sessions
        WHERE user_id = $1 AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4",
        user.uuid(),
        after.map(|after| after.0),
        after.map(|after| after.1),
        PAGE,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ExportedSession {
            id: row.id,
            device_id: row.device_id,
            device_name: row.device_name,
            created_at: Timestamp::from_database(row.created_at),
            updated_at: Timestamp::from_database(row.updated_at),
            authenticated_at: Timestamp::from_database(row.authenticated_at),
            access_token_expires_at: Timestamp::from_database(row.access_token_expires_at),
            refresh_token_expires_at: Timestamp::from_database(row.refresh_token_expires_at),
            expires_at: Timestamp::from_database(row.expires_at),
        })
        .collect())
}

/// A page of `user`'s device tokens, the latest first, after `after`, a token's creation time and
/// ID.
pub(in crate::modules::auth) async fn device_tokens(
    uow: &mut UnitOfWork,
    user: UserId,
    after: Option<(time::OffsetDateTime, uuid::Uuid)>,
) -> Result<Vec<ExportedDeviceToken>, Internal> {
    let rows = sqlx::query!(
        "SELECT id, created_at, expires_at FROM device_tokens
        WHERE user_id = $1 AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4",
        user.uuid(),
        after.map(|after| after.0),
        after.map(|after| after.1),
        PAGE,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ExportedDeviceToken {
            id: row.id,
            created_at: Timestamp::from_database(row.created_at),
            expires_at: Timestamp::from_database(row.expires_at),
        })
        .collect())
}

/// Every code of `user`'s: one at most that proves their address, and their reset codes.
pub(in crate::modules::auth) async fn verification_codes(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<ExportedCode>, Internal> {
    let rows = sqlx::query!(
        r#"SELECT id, purpose::text AS "purpose!", created_at, expires_at FROM verification_codes
        WHERE user_id = $1
        ORDER BY purpose"#,
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(ExportedCode {
                id: row.id,
                purpose: Purpose::from_name(&row.purpose)?,
                created_at: Timestamp::from_database(row.created_at),
                expires_at: Timestamp::from_database(row.expires_at),
            })
        })
        .collect()
}

/// `user`'s authenticator app, one at most, and its setup, one at most.
pub(in crate::modules::auth) async fn apps(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<(Vec<ExportedApp>, Vec<ExportedSetup>), Internal> {
    let apps = sqlx::query!(
        "SELECT id, created_at, updated_at, last_used_step FROM totp_factors WHERE user_id = $1",
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    let setups = sqlx::query!(
        "SELECT id, created_at, expires_at FROM totp_setups WHERE user_id = $1",
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok((
        apps.into_iter()
            .map(|row| ExportedApp {
                id: row.id,
                created_at: Timestamp::from_database(row.created_at),
                updated_at: Timestamp::from_database(row.updated_at),
                last_used_step: row.last_used_step,
            })
            .collect(),
        setups
            .into_iter()
            .map(|row| ExportedSetup {
                id: row.id,
                created_at: Timestamp::from_database(row.created_at),
                expires_at: Timestamp::from_database(row.expires_at),
            })
            .collect(),
    ))
}

/// Every recovery code of `user`'s, ten at most.
pub(in crate::modules::auth) async fn recovery_codes(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<ExportedRecoveryCode>, Internal> {
    let rows = sqlx::query!(
        "SELECT id, created_at FROM recovery_codes WHERE user_id = $1 ORDER BY id",
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ExportedRecoveryCode {
            id: row.id,
            created_at: Timestamp::from_database(row.created_at),
        })
        .collect())
}

/// Every sign-in challenge of `user`'s, which the sign-in limits and their 5 minutes keep few
/// (AUTH-006, AUTH-067).
pub(in crate::modules::auth) async fn challenges(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<ExportedChallenge>, Internal> {
    let rows = sqlx::query!(
        "SELECT id, device_id, device_name, created_at, expires_at FROM sign_in_challenges
        WHERE user_id = $1
        ORDER BY id",
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ExportedChallenge {
            id: row.id,
            device_id: row.device_id.map(DeviceId::from_stored),
            device_name: row.device_name.map(DeviceName::from_stored),
            created_at: Timestamp::from_database(row.created_at),
            expires_at: Timestamp::from_database(row.expires_at),
        })
        .collect())
}
