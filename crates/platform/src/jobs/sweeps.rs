//! `platform`'s sweeps of its own tables (PLATFORM-064): periodic jobs that each delete, every
//! hour, the rows that nothing reads any more, in batches of at most 1,000 rows, a transaction
//! each. Scheduled maintenance isn't audited (DATA-27).

use std::time::Duration;

use super::{JobContext, Jobs, Periodic};
use crate::{db::Database, errors::Internal};

/// How often each sweep runs.
const INTERVAL: Duration = Duration::from_secs(3_600);

/// How long one run of a sweep may take.
const TIME_LIMIT: Duration = Duration::from_secs(300);

/// The most rows one transaction of a sweep deletes.
const BATCH: i64 = 1_000;

/// What a sweep deletes.
#[derive(Clone, Copy, Debug)]
enum Sweep {
    /// The rate-limit buckets whose window started more than a day ago, the longest window any
    /// limit allows, so none it deletes still counts (PLATFORM-049).
    RateLimitBuckets,
    /// The idempotency records older than 24 hours, which a claim treats as absent
    /// (PLATFORM-018).
    IdempotencyRecords,
    /// The dead letters older than their retention (OPS-10).
    DeadLetters(Duration),
}

/// Registers the sweeps in `jobs`, on `database`, deleting dead letters after `retention`.
pub(super) fn register(jobs: &mut Jobs, database: &Database, retention: Duration) {
    for (name, sweep) in [
        (
            "platform.expire_rate_limit_buckets",
            Sweep::RateLimitBuckets,
        ),
        (
            "platform.expire_idempotency_records",
            Sweep::IdempotencyRecords,
        ),
        (
            "platform.expire_dead_letters",
            Sweep::DeadLetters(retention),
        ),
    ] {
        let database = database.clone();
        jobs.periodic.insert(
            name,
            Periodic {
                name,
                interval: INTERVAL,
                time_limit: TIME_LIMIT,
                run: std::sync::Arc::new(move |context| {
                    let database = database.clone();
                    Box::pin(async move { run(&database, &context, sweep).await })
                }),
            },
        );
    }
}

/// Deletes what `sweep` deletes, a batch at a time, until a batch finds fewer than [`BATCH`].
async fn run(database: &Database, context: &JobContext, sweep: Sweep) -> Result<(), Internal> {
    loop {
        let mut uow = database.for_job(context).await?;
        let deleted = match sweep {
            Sweep::RateLimitBuckets => sqlx::query!(
                "DELETE FROM rate_limit_buckets WHERE id IN (
                    SELECT id FROM rate_limit_buckets
                    WHERE window_started_at < now() - interval '1 day'
                    LIMIT $1
                )",
                BATCH,
            )
            .execute(uow.executor())
            .await?
            .rows_affected(),
            Sweep::IdempotencyRecords => sqlx::query!(
                "DELETE FROM idempotency_records WHERE id IN (
                    SELECT id FROM idempotency_records
                    WHERE created_at <= now() - interval '24 hours'
                    LIMIT $1
                )",
                BATCH,
            )
            .execute(uow.executor())
            .await?
            .rows_affected(),
            Sweep::DeadLetters(retention) => sqlx::query!(
                "DELETE FROM jobs WHERE id IN (
                    SELECT id FROM jobs
                    WHERE state = 'dead' AND dead_at <= now() - make_interval(secs => $2)
                    LIMIT $1
                )",
                BATCH,
                retention.as_secs_f64(),
            )
            .execute(uow.executor())
            .await?
            .rows_affected(),
        };
        uow.commit().await?;
        if deleted < BATCH.unsigned_abs() {
            return Ok(());
        }
    }
}
