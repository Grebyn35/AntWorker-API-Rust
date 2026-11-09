//! The jobs of `accounts` (OPS-09), owned by `accounts`:
//! - `accounts.export`, enqueued by an export's request for its user, builds the export's archive
//!   in one unit of work, touching `exports`, `export_entries` and `export_chunks`,
//!   and every table that a personal data contract exports from or writes, then enqueues its
//!   email and publishes its event (ACCOUNTS-023). A second run finds the export ready, and
//!   changes nothing.
//! - `accounts.expire_exports`, every hour, makes failed each export still pending a day after its
//!   request, publishing `export.failed` to its user, and deletes the expired exports of
//!   every user, with what their foreign keys delete, touching `exports` and `events`
//!   (ACCOUNTS-027). It spans every user by nature, so it selects exports by their state and
//!   expiry alone, which `check-tenancy` lets this file do (DATA-16), and scheduled maintenance
//!   isn't audited (DATA-22). A second run finds nothing more to change or delete.

use std::time::Duration;

use platform::{
    db::Database,
    errors::Internal,
    jobs::{Duplicate, Job, JobContext, Jobs},
};
use serde::{Deserialize, Serialize};

use super::{events as published, service::exports::Exports};

/// How often the expired exports are deleted.
const EXPIRE_INTERVAL: Duration = Duration::from_secs(3_600);

/// How long one run of the expiry may take.
const EXPIRE_TIME_LIMIT: Duration = Duration::from_secs(300);

/// The most exports one transaction of the expiry changes or deletes.
const BATCH: i64 = 1_000;

/// How long after its request an export still pending is past being built, in hours: every
/// attempt of its job, and every wait between them, ends within hours (PLATFORM-014).
const PENDING_HOURS: i32 = 24;

/// The payload of `accounts.export`: the export whose archive it builds.
#[derive(Serialize, Deserialize)]
pub(super) struct Build {
    pub(super) export_id: uuid::Uuid,
}

impl Job for Build {
    const KIND: &'static str = "accounts.export";
}

impl Exports {
    /// Registers the export's job and the expiry.
    pub(crate) fn register(&self, jobs: &mut Jobs) -> Result<(), Duplicate> {
        let exports = self.clone();
        jobs.queued(move |context: JobContext, job: Build| {
            let exports = exports.clone();
            async move {
                let last = context.attempt() >= Build::MOST_ATTEMPTS;
                exports.build(&context, job.export_id, last).await
            }
        })?;
        let database = self.database.clone();
        jobs.periodic(
            "accounts.expire_exports",
            EXPIRE_INTERVAL,
            EXPIRE_TIME_LIMIT,
            move |context| {
                let database = database.clone();
                async move { expire(&database, &context).await }
            },
        )
    }
}

/// Makes failed each export still pending a day after its request, then deletes the expired
/// exports of every user, a batch at a time each, with their documents and, with `files`, the
/// rows that hold their files' bytes, which their foreign keys delete.
async fn expire(database: &Database, context: &JobContext) -> Result<(), Internal> {
    loop {
        let mut uow = database.for_job(context).await?;
        let failed = sqlx::query!(
            "UPDATE exports SET state = 'failed'
            WHERE state = 'pending' AND id IN (
                SELECT id FROM exports
                WHERE state = 'pending' AND created_at <= now() - make_interval(hours => $1)
                LIMIT $2
            )
            RETURNING id, user_id",
            PENDING_HOURS,
            BATCH,
        )
        .fetch_all(uow.executor())
        .await?;
        for export in &failed {
            let payload = published::ExportChanged {
                export_id: export.id,
            };
            let user = platform::types::UserId::from_uuid(export.user_id);
            uow.publish_to_user(&published::EXPORT_FAILED, user, &payload)
                .await?;
        }
        uow.commit().await?;
        // A batch shorter than its limit, as when a job built one of its exports meanwhile, ends
        // the loop; the next run takes any left.
        if failed.len() < usize::try_from(BATCH).unwrap_or(usize::MAX) {
            break;
        }
    }
    loop {
        let mut uow = database.for_job(context).await?;
        let deleted = sqlx::query!(
            "DELETE FROM exports WHERE id IN (
                SELECT id FROM exports WHERE expires_at <= now() LIMIT $1
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
