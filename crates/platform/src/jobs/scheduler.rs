//! The scheduler, which every replica runs: it claims the periodic jobs that are due, each under
//! a lease, so each runs once per interval across the fleet (PLATFORM-015).

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::watch;
use tracing::{Instrument, field::Empty};
use uuid::Uuid;

use super::{
    DURATION, JobContext, JobScope, Outcome, Periodic, before, count, failed, release_within,
    renew_every, runs::Runs,
};
use crate::{
    clock::SharedClock,
    db::{ClaimedSchedule, Database},
    errors::{self, Internal},
    telemetry::names,
};

/// How often the scheduler looks for periodic jobs that are due.
const TICK: Duration = Duration::from_secs(10);

/// A run's claim, which a shutdown releases unless the run panics as it is cancelled
/// (PLATFORM-018).
struct Claim {
    name: &'static str,
    claim: Uuid,
}

/// Runs the periodic jobs as they fall due until `stopping` turns true, then waits for the runs
/// in flight until `deadline` passes, and releases those it then cancels, but one that panics as
/// it is cancelled, through [`release_within`] (PLATFORM-018). An insert of the schedules, or a
/// claim, that the database hasn't answered when `deadline` passes is given up then, through
/// [`before`].
pub(super) async fn schedule(
    periodic: Arc<HashMap<&'static str, Periodic>>,
    database: Database,
    clock: SharedClock,
    mut stopping: watch::Receiver<bool>,
    deadline: watch::Receiver<bool>,
) {
    let names: Vec<String> = periodic.keys().map(|name| (*name).to_owned()).collect();
    let mut added = false;
    let mut runs = Runs::new();
    while !*stopping.borrow() {
        if !added {
            let Some(inserted) = before(&deadline, database.add_schedules(&names)).await else {
                break;
            };
            added = match inserted {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(
                        { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                        "can't add the periodic jobs' schedules"
                    );
                    false
                }
            };
        }
        if added {
            let Some(claimed) = before(&deadline, database.claim_schedules(&names)).await else {
                break;
            };
            match claimed {
                Ok(claimed) => {
                    for schedule in claimed {
                        let Some(job) = periodic.get(schedule.name.as_str()).cloned() else {
                            continue;
                        };
                        let claim = Claim {
                            name: job.name,
                            claim: schedule.claim_id,
                        };
                        runs.spawn(
                            claim,
                            run_periodic(job, schedule, database.clone(), clock.clone()),
                        );
                    }
                }
                Err(error) => tracing::warn!(
                    { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                    "can't claim periodic jobs"
                ),
            }
        }
        tokio::select! {
            () = runs.next(), if !runs.is_empty() => {}
            () = clock.sleep(TICK) => {}
            changed = stopping.changed() => if changed.is_err() { break },
        }
    }
    let cancelled = runs.drain(deadline).await;
    release_within(&clock, async {
        for claim in cancelled {
            match database.release_schedule(claim.name, claim.claim).await {
                Ok(released) => count(claim.name, Outcome::Released, released),
                Err(error) => tracing::warn!(
                    { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                    { names::JOB_KIND } = claim.name,
                    "can't release a cancelled periodic job, whose lease runs out instead"
                ),
            }
        }
    })
    .await;
}

/// Runs a periodic job in a span of its own, renewing its lease while it runs and cancelling it
/// at its time limit, then sets its next run one interval after this one began, by its claim, and
/// counts it only when that found the periodic job under the claim. A failed run waits for its
/// next interval too, and its span ends with an error status (PLATFORM-020).
async fn run_periodic(
    job: Periodic,
    schedule: ClaimedSchedule,
    database: Database,
    clock: SharedClock,
) {
    let span = tracing::info_span!(
        parent: None,
        "job",
        { names::OTEL_NAME } = job.name,
        { names::OTEL_STATUS_CODE } = Empty,
        { names::JOB_KIND } = job.name,
        { names::JOB_ID } = %schedule.claim_id,
    );
    run(job, schedule, database, clock).instrument(span).await;
}

async fn run(job: Periodic, schedule: ClaimedSchedule, database: Database, clock: SharedClock) {
    let claim = schedule.claim_id;
    let context = JobContext {
        id: claim,
        attempt: 1,
        scope: JobScope::none(),
    };
    let started = clock.now();
    let result = tokio::select! {
        result = (job.run)(context) => result,
        () = clock.sleep(job.time_limit) => Err(Internal::new("the run passed its time limit")),
        never = renew_every(&clock, || database.renew_schedule(job.name, claim)) => {
            match never {}
        }
    };
    DURATION.observe(
        clock.now().duration_since(started).as_secs_f64(),
        [job.name.into()],
    );
    let outcome = match result {
        Ok(()) => Outcome::Succeeded,
        Err(error) => {
            failed();
            tracing::warn!(
                { names::JOB_KIND } = job.name,
                { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                "periodic job failed, and runs again at its next interval"
            );
            Outcome::Failed
        }
    };
    let finished = database
        .finish_schedule(job.name, claim, schedule.claimed_at, job.interval)
        .await;
    match finished {
        Ok(recorded) => count(job.name, outcome, recorded),
        Err(error) => tracing::warn!(
            { names::EXCEPTION_MESSAGE } = errors::chain(&error),
            "can't record a periodic job's next run, so it runs again once its lease runs out"
        ),
    }
}
