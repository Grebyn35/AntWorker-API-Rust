//! The scheduler, which every replica runs: it claims the periodic jobs that are due, each under
//! a lease, so each runs once per interval across the fleet (OPS-11, PLATFORM-014).

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::watch;
use tracing::Instrument;
use uuid::Uuid;

use super::{
    DURATION, JobContext, JobScope, LEASE, Outcome, Periodic, count, renew_every, runs::Runs,
};
use crate::{
    clock::SharedClock,
    db::{ClaimedSchedule, Database},
    errors::{self, Internal},
    telemetry::names,
};

/// How often the scheduler looks for periodic jobs that are due.
const TICK: Duration = Duration::from_secs(10);

/// A run's claim, which a shutdown releases.
struct Claim {
    name: &'static str,
    claim: Uuid,
}

/// Runs the periodic jobs as they fall due until `stopping` turns true, then waits for the runs
/// in flight until `deadline` passes, and releases those it then cancels (PLATFORM-063).
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
            added = match database.add_schedules(&names).await {
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
            match database.claim_schedules(&names, LEASE).await {
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
    for claim in runs.drain(deadline).await {
        match database.release_schedule(claim.name, claim.claim).await {
            Ok(()) => count(claim.name, Outcome::Released),
            Err(error) => tracing::warn!(
                { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                { names::JOB_KIND } = claim.name,
                "can't release a cancelled periodic job, whose lease runs out instead"
            ),
        }
    }
}

/// Runs a periodic job in a span of its own, renewing its lease while it runs and cancelling it
/// at its time limit, then sets its next run one interval after this one began. A failed run
/// waits for its next interval too.
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
        never = renew_every(&clock, || database.renew_schedule(job.name, claim, LEASE)) => {
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
        Ok(()) => count(job.name, outcome),
        Err(error) => tracing::warn!(
            { names::EXCEPTION_MESSAGE } = errors::chain(&error),
            "can't record a periodic job's next run, so it runs again once its lease runs out"
        ),
    }
}
