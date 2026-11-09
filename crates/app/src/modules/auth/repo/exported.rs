//! The SQL of what a user's export holds of their identities, sessions and device tokens: never a
//! password's or a token's hash (SEC-03), and filtered by the user whose export the job builds
//! (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    personal_data::PAGE,
    types::{Timestamp, UserId},
};

use crate::modules::auth::model::{
    ExportedDeviceToken, ExportedIdentity, ExportedSession, IdentityMethod,
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
        "SELECT id, device_id, created_at, updated_at, authenticated_at, access_token_expires_at,
            refresh_token_expires_at, expires_at
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
