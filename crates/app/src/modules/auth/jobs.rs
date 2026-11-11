//! The jobs of `auth` (OPS-09), owned by `auth`:
//! - `auth.send_verification_code`, enqueued by a registration with email on and by asking for a
//!   code, for the account's user, issues a code that proves the account's email address, in
//!   place of the one before it; enqueued by asking for a password reset, for the account that
//!   holds the address or for no one, a code that resets its password, beside the account's
//!   others; or, enqueued by asking to change the account's address, for the account's user, a
//!   code that changes it to the address its payload names, in place of the one before it. It
//!   reads `users` through `accounts`, touches `verification_codes` and `audit_entries`, and sends
//!   the code at once, to the account's address, or to the one a change asked for, so the code is
//!   kept nowhere but the email (AUTH-030, AUTH-036, AUTH-050, SEC-03). A run for no one does
//!   nothing. A second run, such as after a replica stopped once the email was sent, issues a new
//!   code and sends it too: the address receives two emails. Of two verification codes, or two
//!   change codes, only the second's works, which its owner recognizes by the email's text, since
//!   each says that a newer code replaces it; of two reset codes, either works until it expires or
//!   a reset uses one (OPS-09, D128).
//! - `auth.expire_sessions`, periodic, every hour, deletes the sessions of every user that
//!   stopped lasting, touching `sessions` alone (AUTH-047, D131). It spans every user by nature,
//!   so it selects sessions by expiry alone, which `check-tenancy` lets this file alone do
//!   (DATA-16). Scheduled maintenance isn't audited (DATA-22). A run skips a row that a request
//!   holds, which the next deletes; a run right after one that skipped none finds nothing more to
//!   delete.
//! - `auth.expire_unfinished`, periodic, every hour, deletes the setups of authenticator apps and
//!   the sign-in challenges of every user that expired, touching `totp_setups` and
//!   `sign_in_challenges` alone, by expiry alone, as `auth.expire_sessions` does (AUTH-067). A
//!   run that meets a row that a request holds ends there, and the next deletes the rest.
//! - `auth.reencrypt_secrets`, periodic, every hour, stores every authenticator app's secret that
//!   another version of the key ring holds anew under the current one, touching `totp_factors`
//!   alone, by key version alone (AUTH-066). It spans every user by nature. An app whose row a
//!   request holds is left for the next run.

use std::time::Duration;

use platform::{
    db::{Database, is_lock_timeout},
    encryption::{Binding, Keyring, Sealed},
    errors::Internal,
    jobs::{Duplicate, Job, JobContext, Jobs},
    types::EmailAddress,
};
use serde::{Deserialize, Serialize};

use super::{model::Purpose, service::Auth};

/// How often each periodic job of `auth` runs: the sweep of sessions, the sweep of setups and
/// challenges, and the re-encryption of apps' secrets.
const EXPIRE_INTERVAL: Duration = Duration::from_secs(3_600);

/// How long one run of each may take.
const EXPIRE_TIME_LIMIT: Duration = Duration::from_secs(300);

/// The most rows of each table that one transaction of each deletes or encrypts again.
const BATCH: i64 = 1_000;

/// The payload of `auth.send_verification_code`: what the code proves, or resets, and, for a
/// change of the address, the address asked for, which a payload of a release before it lacks
/// (OPS-09).
#[derive(Serialize, Deserialize)]
pub(super) struct SendCode {
    pub(super) purpose: Purpose,
    /// The address that a change code goes to, and changes the account's address to (AUTH-050).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) email: Option<EmailAddress>,
}

impl Job for SendCode {
    const KIND: &'static str = "auth.send_verification_code";
    /// As many as `platform.send_email`'s, so a mail server down for a few hours delays a code
    /// rather than losing it (PLATFORM-021).
    const MOST_ATTEMPTS: u32 = 12;
    const TIME_LIMIT: Duration = Duration::from_secs(60);
}

/// Registers in `jobs` the jobs of `auth`, which `auth` runs.
pub(crate) fn register(auth: &std::sync::Arc<Auth>, jobs: &mut Jobs) -> Result<(), Duplicate> {
    let sender = auth.clone();
    jobs.queued(move |context: JobContext, job: SendCode| {
        let auth = sender.clone();
        async move { auth.issue_code(&context, job).await }
    })?;
    let database = auth.database();
    jobs.periodic(
        "auth.expire_sessions",
        EXPIRE_INTERVAL,
        EXPIRE_TIME_LIMIT,
        move |context| {
            let database = database.clone();
            async move { expire(&database, &context).await }
        },
    )?;
    let database = auth.database();
    jobs.periodic(
        "auth.expire_unfinished",
        EXPIRE_INTERVAL,
        EXPIRE_TIME_LIMIT,
        move |context| {
            let database = database.clone();
            async move { expire_unfinished(&database, &context).await }
        },
    )?;
    let database = auth.database();
    let keyring = auth.keyring();
    jobs.periodic(
        "auth.reencrypt_secrets",
        EXPIRE_INTERVAL,
        EXPIRE_TIME_LIMIT,
        move |context| {
            let database = database.clone();
            let keyring = keyring.clone();
            async move { reencrypt(&database, &keyring, &context).await }
        },
    )
}

/// Deletes the sessions of every user that stopped lasting, a batch at a time. A session's
/// refresh expiry never passes its absolute lifetime (AUTH-010), so it stops lasting when its
/// refresh token expires, which the index on `refresh_token_expires_at` finds. A row that a
/// request holds locked is skipped, so the sweep waits for none and deadlocks with none, and
/// goes at the next run (DATA-20).
async fn expire(database: &Database, context: &JobContext) -> Result<(), Internal> {
    loop {
        let mut uow = database.for_job(context).await?;
        let deleted = sqlx::query!(
            "DELETE FROM sessions WHERE id IN (
                SELECT id FROM sessions WHERE refresh_token_expires_at <= now()
                LIMIT $1 FOR UPDATE SKIP LOCKED
            )",
            BATCH,
        )
        .execute(uow.executor())
        .await?
        .rows_affected();
        uow.commit().await?;
        if deleted < BATCH.unsigned_abs() {
            return Ok(());
        }
    }
}

/// Deletes the setups of apps and the sign-in challenges of every user that expired, a batch at a
/// time. `app` holds no `UPDATE` on either table, which `SELECT ... FOR UPDATE` asks for, so a
/// batch can't skip the rows that a request holds (DATA-19): it waits for no lock, but the
/// millisecond that is the least `lock_timeout` Postgres takes, so a request that holds one of its
/// rows, such as a reset that deletes the user's challenges, never waits for the sweep while the
/// sweep waits for it, and the run ends there, leaving the rest to the next (AUTH-067, DATA-20).
async fn expire_unfinished(database: &Database, context: &JobContext) -> Result<(), Internal> {
    loop {
        let mut uow = database.for_job(context).await?;
        uow.wait_for_locks_at_most(Duration::from_millis(1)).await?;
        let deleted = async {
            let setups = sqlx::query!(
                "DELETE FROM totp_setups WHERE id IN (
                    SELECT id FROM totp_setups WHERE expires_at <= now() ORDER BY id LIMIT $1
                )",
                BATCH,
            )
            .execute(uow.executor())
            .await?
            .rows_affected();
            let challenges = sqlx::query!(
                "DELETE FROM sign_in_challenges WHERE id IN (
                    SELECT id FROM sign_in_challenges WHERE expires_at <= now()
                    ORDER BY id LIMIT $1
                )",
                BATCH,
            )
            .execute(uow.executor())
            .await?
            .rows_affected();
            Ok::<_, sqlx::Error>((setups, challenges))
        }
        .await;
        let (setups, challenges) = match deleted {
            Ok(counts) => counts,
            Err(error) if is_lock_timeout(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        uow.commit().await?;
        if setups < BATCH.unsigned_abs() && challenges < BATCH.unsigned_abs() {
            return Ok(());
        }
    }
}

/// Stores every app's secret that another version of `keyring` holds anew under its current
/// version, a batch at a time, skipping an app that a request holds, which the next run takes
/// (AUTH-066, DATA-20).
async fn reencrypt(
    database: &Database,
    keyring: &Keyring,
    context: &JobContext,
) -> Result<(), Internal> {
    loop {
        let mut uow = database.for_job(context).await?;
        let rows = sqlx::query!(
            "SELECT id, key_version, secret FROM totp_factors
            WHERE key_version <> $1
            LIMIT $2 FOR UPDATE SKIP LOCKED",
            keyring.current(),
            BATCH,
        )
        .fetch_all(uow.executor())
        .await?;
        let count = rows.len();
        for row in rows {
            let binding = Binding::new("totp_factors", "secret", row.id);
            let stored = Sealed {
                version: row.key_version,
                bytes: row.secret,
            };
            let sealed = keyring.seal(&binding, keyring.open(&binding, &stored)?.expose())?;
            sqlx::query!(
                "UPDATE totp_factors SET key_version = $2, secret = $3 WHERE id = $1",
                row.id,
                sealed.version,
                sealed.bytes,
            )
            .execute(uow.executor())
            .await?;
        }
        uow.commit().await?;
        if count < usize::try_from(BATCH).unwrap_or(usize::MAX) {
            return Ok(());
        }
    }
}
