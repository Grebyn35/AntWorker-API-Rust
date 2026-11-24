//! The dispatcher, which every replica runs: it claims runnable queued jobs of the kinds its
//! registry holds, and runs at most 4 at once (PLATFORM-016).

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::watch;
use tracing::{Instrument, field::Empty};
use uuid::Uuid;

use super::{
    DURATION, JobContext, JobFailure, JobScope, Outcome, Queued, before, count, failed,
    release_within, renew_every, runs::Runs,
};
use crate::{
    clock::SharedClock,
    db::{ClaimedJob, Database},
    errors::{self, Internal},
    random,
    telemetry::{self, names},
    types::{UserId, WorkspaceId},
};

/// The most runs a replica has in flight at once.
const MOST_RUNNING: usize = 4;

/// How long the dispatcher waits before it polls again, once it found nothing to claim.
const POLL: Duration = Duration::from_secs(1);

/// The wait before the first retry, which each retry after it doubles.
const FIRST_RETRY: Duration = Duration::from_secs(10);

/// The longest wait before a retry.
const LONGEST_RETRY: Duration = Duration::from_secs(3_600);

/// A run's claim, which a shutdown releases.
struct Claim {
    kind: &'static str,
    id: Uuid,
    claim: Uuid,
}

/// Claims and runs jobs until `stopping` turns true, then waits for the runs in flight until
/// `deadline` passes, and releases those it then cancels, through [`release_within`]
/// (PLATFORM-018). A claim that the database hasn't answered when `deadline` passes is given up
/// then, through [`before`].
pub(super) async fn dispatch(
    queued: Arc<HashMap<&'static str, Queued>>,
    database: Database,
    clock: SharedClock,
    mut stopping: watch::Receiver<bool>,
    deadline: watch::Receiver<bool>,
) {
    let kinds: Vec<String> = queued.keys().map(|kind| (*kind).to_owned()).collect();
    let mut runs = Runs::new();
    while !*stopping.borrow() {
        let room = MOST_RUNNING.saturating_sub(runs.len());
        if room > 0 && !kinds.is_empty() {
            let Some(claimed) = before(&deadline, database.claim_jobs(&kinds, room)).await else {
                break;
            };
            match claimed {
                Ok(claimed) => {
                    for job in claimed {
                        let Some(kind) = queued.get(job.kind.as_str()).cloned() else {
                            continue;
                        };
                        let claim = Claim {
                            kind: kind.kind,
                            id: job.id,
                            claim: job.claim_id,
                        };
                        runs.spawn(claim, run_job(kind, job, database.clone(), clock.clone()));
                    }
                }
                Err(error) => tracing::warn!(
                    { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                    "can't claim jobs"
                ),
            }
        }
        // A run that ends makes room to claim again at once; otherwise the next poll does.
        tokio::select! {
            () = runs.next(), if !runs.is_empty() => {}
            () = clock.sleep(POLL) => {}
            changed = stopping.changed() => if changed.is_err() { break },
        }
    }
    let cancelled = runs.drain(deadline).await;
    release_within(&clock, async {
        for claim in cancelled {
            match database.release_job(claim.id, claim.claim).await {
                Ok(()) => count(claim.kind, Outcome::Released),
                Err(error) => tracing::warn!(
                    { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                    { names::JOB_KIND } = claim.kind,
                    { names::JOB_ID } = %claim.id,
                    "can't release a cancelled job, whose lease runs out instead"
                ),
            }
        }
    })
    .await;
}

/// Runs `job` in a span of its own, linked to the span that enqueued it (OBS-07), renewing its
/// lease while it runs and cancelling it at its time limit (PLATFORM-017), then writes its
/// outcome by its claim, so a run whose lease ran out, while another replica claimed the job,
/// changes nothing. The span of a run that fails, or that moves its job to the dead-letter
/// state, ends with an error status (PLATFORM-020).
pub(super) async fn run_job(
    queued: Queued,
    job: ClaimedJob,
    database: Database,
    clock: SharedClock,
) {
    let attempt = u32::try_from(job.attempts).unwrap_or(u32::MAX);
    let span = tracing::info_span!(
        parent: None,
        "job",
        { names::OTEL_NAME } = queued.kind,
        { names::OTEL_KIND } = "consumer",
        { names::OTEL_STATUS_CODE } = Empty,
        { names::JOB_KIND } = queued.kind,
        { names::JOB_ID } = %job.id,
        { names::JOB_ATTEMPT } = i64::from(attempt),
    );
    if let Some(traceparent) = &job.traceparent {
        telemetry::link(&span, traceparent);
    }
    run(queued, job, attempt, database, clock)
        .instrument(span)
        .await;
}

async fn run(
    queued: Queued,
    job: ClaimedJob,
    attempt: u32,
    database: Database,
    clock: SharedClock,
) {
    let (id, claim) = (job.id, job.claim_id);
    let outcome = if attempt > queued.most_attempts {
        // The replica that ran its last attempt stopped before the attempt ended.
        let failure = "the replica of its last attempt stopped during it";
        failed();
        tracing::error!(
            { names::JOB_KIND } = queued.kind,
            { names::EXCEPTION_MESSAGE } = failure,
            "job dead-lettered"
        );
        database
            .kill_job(id, claim, failure)
            .await
            .map(|()| Outcome::DeadLettered)
    } else {
        let context = JobContext {
            id,
            attempt,
            scope: JobScope::proven(
                job.user_id.map(UserId::from_uuid),
                job.workspace_id.map(WorkspaceId::from_uuid),
            ),
        };
        let started = clock.now();
        let result = tokio::select! {
            result = (queued.run)(context, job.payload) => result,
            () = clock.sleep(queued.time_limit) => Err(JobFailure::Transient(Internal::new(
                "the run passed its time limit",
            ))),
            never = renew_every(&clock, || database.renew_job(id, claim)) => match never {},
        };
        if result.is_err() {
            failed();
        }
        DURATION.observe(
            clock.now().duration_since(started).as_secs_f64(),
            [queued.kind.into()],
        );
        end(&queued, id, claim, attempt, result, &database).await
    };
    match outcome {
        Ok(outcome) => count(queued.kind, outcome),
        Err(error) => tracing::warn!(
            { names::EXCEPTION_MESSAGE } = errors::chain(&error),
            "can't record a job's outcome, so it runs again once its lease runs out"
        ),
    }
}

/// Writes the outcome of a run that ended with `result` (PLATFORM-014).
async fn end(
    queued: &Queued,
    id: Uuid,
    claim: Uuid,
    attempt: u32,
    result: Result<(), JobFailure>,
    database: &Database,
) -> Result<Outcome, Internal> {
    let (error, permanent) = match result {
        Ok(()) => {
            database.succeed_job(id, claim).await?;
            return Ok(Outcome::Succeeded);
        }
        Err(JobFailure::Transient(error)) => (error, false),
        Err(JobFailure::Permanent(error)) => (error, true),
    };
    let failure = errors::chain(&error);
    if permanent || attempt >= queued.most_attempts {
        tracing::error!(
            { names::JOB_KIND } = queued.kind,
            { names::EXCEPTION_MESSAGE } = failure,
            "job dead-lettered"
        );
        database.kill_job(id, claim, &failure).await?;
        return Ok(Outcome::DeadLettered);
    }
    let delay = backoff(attempt)?;
    tracing::warn!(
        { names::JOB_KIND } = queued.kind,
        { names::EXCEPTION_MESSAGE } = failure,
        "job failed, and runs again later"
    );
    database.retry_job(id, claim, delay, &failure).await?;
    Ok(Outcome::Retried)
}

/// The wait before the retry that follows attempt `attempt`: a random time between half of and
/// all of the lesser of 10 seconds times 2 to the power `attempt − 1`, and an hour, so retries
/// spread out rather than arrive together (PLATFORM-014).
fn backoff(attempt: u32) -> Result<Duration, Internal> {
    let doublings = attempt.saturating_sub(1).min(16);
    let ceiling = FIRST_RETRY
        .saturating_mul(1 << doublings)
        .min(LONGEST_RETRY);
    let share = f64::from(u32::from_le_bytes(random::bytes::<4>()?)) / f64::from(u32::MAX);
    Ok(ceiling.div_f64(2.0).mul_f64(1.0 + share))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_014_each_retry_waits_between_half_of_and_all_of_its_ceiling() {
        for (attempt, ceiling) in [
            (1, 10),
            (2, 20),
            (3, 40),
            (9, 2_560),
            (10, 3_600),
            (30, 3_600),
        ] {
            let ceiling = Duration::from_secs(ceiling);
            for _ in 0..100 {
                let delay = backoff(attempt).unwrap();
                assert!(delay >= ceiling / 2, "{delay:?} for attempt {attempt}");
                assert!(delay <= ceiling, "{delay:?} for attempt {attempt}");
            }
        }
    }

    #[test]
    fn platform_021_the_11_retries_of_12_attempts_wait_half_of_to_all_of_12_310_seconds() {
        // The longest that each retry of an email's job can wait, as the platform spec's Sending
        // email sums them.
        let longest = [10, 20, 40, 80, 160, 320, 640, 1_280, 2_560, 3_600, 3_600];
        let all = Duration::from_secs(longest.iter().sum());

        for _ in 0..100 {
            let mut waited = Duration::ZERO;
            for (attempt, longest) in (1..12).zip(longest) {
                let wait = backoff(attempt).unwrap();
                let longest = Duration::from_secs(longest);
                assert!(
                    wait >= longest / 2 && wait <= longest,
                    "{wait:?} after {attempt}"
                );
                waited += wait;
            }
            assert!(waited >= all / 2 && waited <= all, "{waited:?}");
        }
        assert_eq!(all, Duration::from_secs(12_310));
    }
}
