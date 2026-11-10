//! The jobs of `auth` (OPS-09), owned by `auth`:
//! - `auth.send_verification_code`, enqueued by a registration with email on and by asking for a
//!   code, for the account's user, issues a code that proves the account's email address, in
//!   place of the one before it, or, enqueued by asking for a password reset, for the account that
//!   holds the address or for no one, a code that resets its password, beside the account's
//!   others, reading `users` through `accounts`, touching `verification_codes` and
//!   `audit_entries`, and sends it to the address at once, so the code is kept nowhere but the
//!   email (AUTH-030, AUTH-036, SEC-03). A run for no one does nothing. A second run, such as
//!   after a replica stopped once the email was sent, issues a new code and sends it too: the
//!   account receives two emails. Of two verification codes only the second's works, which its
//!   owner recognizes by the email's text, since each says that a newer code replaces it; of two
//!   reset codes, either works until it expires or a reset uses one (OPS-09, D128).
//! - `auth.expire_sessions`, periodic, every hour, deletes the sessions of every user that
//!   stopped lasting, touching `sessions` alone (AUTH-047, D131). It spans every user by nature,
//!   so it selects sessions by expiry alone, which `check-tenancy` lets this file alone do
//!   (DATA-16). Scheduled maintenance isn't audited (DATA-22). A second run finds nothing more to
//!   delete.

use std::time::Duration;

use platform::{
    db::Database,
    errors::Internal,
    jobs::{Duplicate, Job, JobContext, Jobs},
};
use serde::{Deserialize, Serialize};

use super::{model::Purpose, service::Auth};

/// How often the sweep of sessions runs.
const EXPIRE_INTERVAL: Duration = Duration::from_secs(3_600);

/// How long one run of the sweep may take.
const EXPIRE_TIME_LIMIT: Duration = Duration::from_secs(300);

/// The most sessions one transaction of the sweep deletes.
const BATCH: i64 = 1_000;

/// The payload of `auth.send_verification_code`: what the code proves, or resets.
#[derive(Serialize, Deserialize)]
pub(super) struct SendCode {
    pub(super) purpose: Purpose,
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
