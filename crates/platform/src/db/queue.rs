//! The job queue's statements that run outside any transaction, each committing at once: the
//! claims of jobs and periodic jobs, the renewals of their leases, and their outcomes (the
//! platform spec's Jobs). Every statement after a claim names the claim's ID, so a run whose
//! lease ran out, while another replica claimed the job, changes nothing.

use std::time::Duration;

use time::OffsetDateTime;
use uuid::Uuid;

use super::Database;
use crate::errors::Internal;

/// A job that a dispatcher claimed.
#[derive(Debug)]
pub(crate) struct ClaimedJob {
    /// The job's ID.
    pub(crate) id: Uuid,
    /// Its kind.
    pub(crate) kind: String,
    /// Its payload.
    pub(crate) payload: serde_json::Value,
    /// The user it runs for, if any.
    pub(crate) user_id: Option<Uuid>,
    /// The workspace it runs for, if any.
    pub(crate) workspace_id: Option<Uuid>,
    /// The trace context of the span that enqueued it.
    pub(crate) traceparent: Option<String>,
    /// Its attempts, this one included.
    pub(crate) attempts: i32,
    /// The claim.
    pub(crate) claim_id: Uuid,
}

/// A periodic job that a scheduler claimed.
#[derive(Debug)]
pub(crate) struct ClaimedSchedule {
    /// The periodic job's name.
    pub(crate) name: String,
    /// The claim.
    pub(crate) claim_id: Uuid,
    /// When the claim began, by the database's clock, from which the next run counts.
    pub(crate) claimed_at: OffsetDateTime,
}

impl Database {
    /// Claims at most `limit` runnable jobs of `kinds`, the longest-waiting first, each with a
    /// lease of `lease` and one more attempt.
    pub(crate) async fn claim_jobs(
        &self,
        kinds: &[String],
        limit: usize,
        lease: Duration,
    ) -> Result<Vec<ClaimedJob>, Internal> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = sqlx::query!(
            r#"UPDATE jobs SET
                claim_id = gen_random_uuid(),
                lease_expires_at = now() + make_interval(secs => $3),
                attempts = attempts + 1
            WHERE id IN (
                SELECT id FROM jobs
                WHERE state = 'waiting' AND run_at <= now() AND kind = ANY($1)
                    AND (lease_expires_at IS NULL OR lease_expires_at <= now())
                ORDER BY run_at
                LIMIT $2
                FOR UPDATE SKIP LOCKED
            )
            RETURNING id, kind, payload, user_id, workspace_id, traceparent, attempts,
                claim_id AS "claim_id!""#,
            kinds,
            limit,
            lease.as_secs_f64(),
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| ClaimedJob {
                id: row.id,
                kind: row.kind,
                payload: row.payload,
                user_id: row.user_id,
                workspace_id: row.workspace_id,
                traceparent: row.traceparent,
                attempts: row.attempts,
                claim_id: row.claim_id,
            })
            .collect())
    }

    /// Renews the lease of the job that `claim` holds to `lease` from now.
    pub(crate) async fn renew_job(
        &self,
        id: Uuid,
        claim: Uuid,
        lease: Duration,
    ) -> Result<(), Internal> {
        sqlx::query!(
            "UPDATE jobs SET lease_expires_at = now() + make_interval(secs => $3)
            WHERE id = $1 AND claim_id = $2",
            id,
            claim,
            lease.as_secs_f64(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Deletes the job that `claim` holds, which succeeded.
    pub(crate) async fn succeed_job(&self, id: Uuid, claim: Uuid) -> Result<(), Internal> {
        sqlx::query!(
            "DELETE FROM jobs WHERE id = $1 AND claim_id = $2",
            id,
            claim
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ends the claim of the job that `claim` holds, which failed with `failure`, and lets it
    /// run again after `delay`.
    pub(crate) async fn retry_job(
        &self,
        id: Uuid,
        claim: Uuid,
        delay: Duration,
        failure: &str,
    ) -> Result<(), Internal> {
        sqlx::query!(
            "UPDATE jobs SET
                run_at = now() + make_interval(secs => $3),
                claim_id = NULL,
                lease_expires_at = NULL,
                failure = $4
            WHERE id = $1 AND claim_id = $2",
            id,
            claim,
            delay.as_secs_f64(),
            failure,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Moves the job that `claim` holds, which failed with `failure`, to the dead-letter state.
    pub(crate) async fn kill_job(
        &self,
        id: Uuid,
        claim: Uuid,
        failure: &str,
    ) -> Result<(), Internal> {
        sqlx::query!(
            "UPDATE jobs SET
                state = 'dead',
                dead_at = now(),
                claim_id = NULL,
                lease_expires_at = NULL,
                failure = $3
            WHERE id = $1 AND claim_id = $2",
            id,
            claim,
            failure,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ends the claim of the job that `claim` holds without counting its attempt, so another
    /// replica runs it at once.
    pub(crate) async fn release_job(&self, id: Uuid, claim: Uuid) -> Result<(), Internal> {
        sqlx::query!(
            "UPDATE jobs SET claim_id = NULL, lease_expires_at = NULL, attempts = attempts - 1
            WHERE id = $1 AND claim_id = $2",
            id,
            claim,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// How long the longest-waiting runnable job has waited past its `run_at`, or zero when none
    /// waits.
    pub(crate) async fn job_delay(&self) -> Result<Duration, Internal> {
        let seconds = sqlx::query_scalar!(
            r#"SELECT coalesce(extract(epoch FROM now() - min(run_at)), 0)::float8 AS "seconds!"
            FROM jobs
            WHERE state = 'waiting' AND run_at <= now()
                AND (lease_expires_at IS NULL OR lease_expires_at <= now())"#,
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(Duration::try_from_secs_f64(seconds.max(0.0)).unwrap_or_default())
    }

    /// Inserts the schedule of each periodic job in `names` that has none, due at once.
    pub(crate) async fn add_schedules(&self, names: &[String]) -> Result<(), Internal> {
        sqlx::query!(
            "INSERT INTO job_schedules (name, next_run_at)
            SELECT name, now() FROM unnest($1::text[]) AS name
            ON CONFLICT ON CONSTRAINT job_schedules_name_key DO NOTHING",
            names,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Claims every due periodic job of `names` that no replica holds, each with a lease of
    /// `lease`.
    pub(crate) async fn claim_schedules(
        &self,
        names: &[String],
        lease: Duration,
    ) -> Result<Vec<ClaimedSchedule>, Internal> {
        let rows = sqlx::query!(
            r#"UPDATE job_schedules SET
                claim_id = gen_random_uuid(),
                lease_expires_at = now() + make_interval(secs => $2)
            WHERE id IN (
                SELECT id FROM job_schedules
                WHERE name = ANY($1) AND next_run_at <= now()
                    AND (lease_expires_at IS NULL OR lease_expires_at <= now())
                FOR UPDATE SKIP LOCKED
            )
            RETURNING name, claim_id AS "claim_id!", now() AS "claimed_at!""#,
            names,
            lease.as_secs_f64(),
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| ClaimedSchedule {
                name: row.name,
                claim_id: row.claim_id,
                claimed_at: row.claimed_at,
            })
            .collect())
    }

    /// Renews the lease of the periodic job that `claim` holds to `lease` from now.
    pub(crate) async fn renew_schedule(
        &self,
        name: &str,
        claim: Uuid,
        lease: Duration,
    ) -> Result<(), Internal> {
        sqlx::query!(
            "UPDATE job_schedules SET lease_expires_at = now() + make_interval(secs => $3)
            WHERE name = $1 AND claim_id = $2",
            name,
            claim,
            lease.as_secs_f64(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ends the claim of the periodic job that `claim` holds, whose run ended, and sets its next
    /// run `interval` after its claim began, at `claimed_at`.
    pub(crate) async fn finish_schedule(
        &self,
        name: &str,
        claim: Uuid,
        claimed_at: OffsetDateTime,
        interval: Duration,
    ) -> Result<(), Internal> {
        sqlx::query!(
            "UPDATE job_schedules SET
                next_run_at = $3::timestamptz + make_interval(secs => $4),
                claim_id = NULL,
                lease_expires_at = NULL
            WHERE name = $1 AND claim_id = $2",
            name,
            claim,
            claimed_at,
            interval.as_secs_f64(),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ends the claim of the periodic job that `claim` holds, leaving it due, so another replica
    /// runs it at once.
    pub(crate) async fn release_schedule(&self, name: &str, claim: Uuid) -> Result<(), Internal> {
        sqlx::query!(
            "UPDATE job_schedules SET claim_id = NULL, lease_expires_at = NULL
            WHERE name = $1 AND claim_id = $2",
            name,
            claim,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
