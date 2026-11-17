//! The SQL of `client_telemetry` (ARCH-10). Every query reads or writes the row of one user, the
//! caller (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{Timestamp, UserId, Version},
};

use super::model::{Current, Exported, TelemetryConsent};

/// The foreign key that refuses the row of a user whose account a deletion took (ERR-05).
const CONSENT_OF_USER: &str = "telemetry_consents_user_id_fkey";

/// The consent of `user`, with its version, or `None` when they never chose.
pub(super) async fn current(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Option<Current>, Internal> {
    let row = sqlx::query!(
        "SELECT consented, version FROM telemetry_consents WHERE user_id = $1",
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Current {
        consent: TelemetryConsent {
            consented: row.consented,
        },
        version: Version::from_database(row.version),
    }))
}

/// Inserts the row of `user`'s choice no one made, `consented` false at version 0, unless they
/// have a row, and returns whether `user` is there: `false` once a deletion of their account
/// committed, whose foreign key refuses the row (ERR-05).
pub(super) async fn insert_unchosen(uow: &mut UnitOfWork, user: UserId) -> Result<bool, Internal> {
    let inserted = sqlx::query!(
        "INSERT INTO telemetry_consents (user_id) VALUES ($1) ON CONFLICT (user_id) DO NOTHING",
        user.uuid(),
    )
    .execute(uow.executor())
    .await;
    match inserted {
        Ok(_) => Ok(true),
        Err(error)
            if error
                .as_database_error()
                .and_then(|error| error.constraint())
                .is_some_and(|constraint| constraint == CONSENT_OF_USER) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

/// Locks the row of `user` `FOR UPDATE`, and returns whether it is there: `false` once a deletion
/// of their account took it.
pub(super) async fn lock(uow: &mut UnitOfWork, user: UserId) -> Result<bool, Internal> {
    let locked = sqlx::query_scalar!(
        "SELECT id FROM telemetry_consents WHERE user_id = $1 FOR UPDATE",
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(locked.is_some())
}

/// Stores `consented` as the consent of `user`, when its version is one of `versions`, or
/// whatever it is when there are none, and returns the consent as it then is, or `None` when it
/// didn't (API-13, DATA-18).
pub(super) async fn set(
    uow: &mut UnitOfWork,
    user: UserId,
    consented: bool,
    versions: Option<&[Version]>,
) -> Result<Option<Current>, Internal> {
    let versions: Option<Vec<i64>> =
        versions.map(|versions| versions.iter().map(|version| version.value()).collect());
    let row = sqlx::query!(
        "UPDATE telemetry_consents SET consented = $2
        WHERE user_id = $1 AND ($3::bigint[] IS NULL OR version = ANY($3))
        RETURNING consented, version",
        user.uuid(),
        consented,
        versions.as_deref(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Current {
        consent: TelemetryConsent {
            consented: row.consented,
        },
        version: Version::from_database(row.version),
    }))
}

/// The row of `user`, if they ever chose, as their export holds it (CLIENT_TELEMETRY-003).
pub(super) async fn exported(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Option<Exported>, Internal> {
    let row = sqlx::query!(
        "SELECT consented, created_at, updated_at FROM telemetry_consents WHERE user_id = $1",
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| Exported {
        consented: row.consented,
        created_at: Timestamp::from_database(row.created_at),
        updated_at: Timestamp::from_database(row.updated_at),
    }))
}
