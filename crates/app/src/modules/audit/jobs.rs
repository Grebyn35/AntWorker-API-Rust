//! The periodic job `audit.expire_entries` (OPS-09, AUDIT-011): owned by `audit`, run every hour,
//! and deleting from `deletable_audit_entries` alone, whose entries' clients go with them through
//! their foreign key. It spans every log by nature, so it selects entries by their age alone,
//! which `check-tenancy` lets this file alone do (DATA-16). Scheduled maintenance isn't audited
//! (DATA-22). A run deletes batches, the oldest first, until one deletes fewer than a batch, or
//! until it meets an entry that another transaction holds, which a later run deletes. No entry's
//! `created_at` ever changes, since `app` updates no entry and a foreign key's action sets only an
//! actor or a target to null, so none that a batch selects becomes younger while the batch waits
//! (DATA-20).

use std::time::Duration;

use platform::{
    db::{Database, is_lock_timeout},
    errors::Internal,
    jobs::{Duplicate, JobContext, Jobs},
};

use super::model::Settings;

/// How often the job runs.
const INTERVAL: Duration = Duration::from_secs(3_600);

/// How long one run of the job may take.
const TIME_LIMIT: Duration = Duration::from_secs(300);

/// The most entries one transaction of the job deletes.
const BATCH: i64 = 1_000;

/// Retention: what deletes each log's entries once the deployment's retention has passed
/// (AUDIT-011).
pub(crate) struct Retention {
    database: Database,
    settings: Settings,
}

impl Retention {
    /// Retention on `database`, with `settings`.
    pub(crate) fn new(database: Database, settings: Settings) -> Self {
        Self { database, settings }
    }

    /// Registers the job.
    pub(crate) fn register(&self, jobs: &mut Jobs) -> Result<(), Duplicate> {
        let database = self.database.clone();
        let days = self.settings.retention_days;
        jobs.periodic(
            "audit.expire_entries",
            INTERVAL,
            TIME_LIMIT,
            move |context| {
                let database = database.clone();
                async move { expire(&database, days, &context).await }
            },
        )
    }
}

/// Deletes the entries of every log recorded more than `days` days ago, a batch at a time, the
/// oldest first. A row lock taken through `deletable_audit_entries` asks for `UPDATE` on it, which
/// `app` doesn't hold (AUDIT-005), so a batch can't skip the entries that another transaction
/// holds, such as an account's deletion, which sets the actor of entries to null: it waits for no
/// lock but the millisecond that is the least `lock_timeout` Postgres takes, so that transaction
/// never waits for the job longer than that while the job waits for it, and the run ends there,
/// rolling its batch back, and leaving the rest to a later run (DATA-20).
async fn expire(database: &Database, days: u32, context: &JobContext) -> Result<(), Internal> {
    let days = i32::try_from(days).map_err(Internal::new)?;
    loop {
        let mut uow = database.for_job(context).await?;
        uow.wait_for_locks_at_most(Duration::from_millis(1)).await?;
        // The batch's IDs come first, as an array, so the deletion finds each by its key: with
        // `IN`, Postgres may join the batch with every entry that the view shows, and read them
        // all again for each batch. The days are counted as hours, which no session's time zone
        // moves, as the view counts its own (AUDIT-005).
        let deleted = sqlx::query!(
            "DELETE FROM deletable_audit_entries WHERE id = ANY(ARRAY(
                SELECT id FROM deletable_audit_entries
                WHERE created_at < now() - make_interval(hours => $1 * 24)
                ORDER BY created_at LIMIT $2
            ))",
            days,
            BATCH,
        )
        .execute(uow.executor())
        .await;
        let deleted = match deleted {
            Ok(done) => done.rows_affected(),
            Err(error) if is_lock_timeout(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        uow.commit().await?;
        if deleted < BATCH.unsigned_abs() {
            return Ok(());
        }
    }
}
