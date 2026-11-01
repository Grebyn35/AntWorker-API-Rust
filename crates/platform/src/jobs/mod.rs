//! Background jobs (the platform spec's Jobs, D13): queued jobs, which a unit of work enqueues
//! and every replica's dispatcher runs at least once, and periodic jobs, which the replicas'
//! schedulers run once per interval across the fleet.

mod dispatcher;
mod runs;
mod scheduler;
mod sweeps;

use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use serde::{Serialize, de::DeserializeOwned};
use tokio::sync::watch;
use uuid::Uuid;

use crate::{
    authn::BoxFuture,
    clock::SharedClock,
    db::{Database, UnitOfWork},
    errors::Internal,
    telemetry::{
        self,
        metrics::{Counter, Gauge, Histogram},
        names,
    },
    types::{UserId, WorkspaceId},
};

/// How long a claim lasts unless its run renews it.
const LEASE: Duration = Duration::from_secs(60);

/// How often a run renews its lease.
const RENEWAL: Duration = Duration::from_secs(20);

/// How often each replica records how long the longest-waiting job has waited.
const DELAY_EVERY: Duration = Duration::from_secs(15);

static RUNS: Counter<2> = Counter::new(
    "platform.job_runs",
    "{run}",
    "Job runs, by the job's kind and how each ended: succeeded, retried, dead_lettered, failed \
     for a periodic run, or released at shutdown (PLATFORM-065).",
    [names::JOB_KIND, names::JOB_OUTCOME],
);

static DURATION: Histogram<1> = Histogram::new(
    "platform.job_duration",
    "s",
    "How long each job run took, by the job's kind.",
    [names::JOB_KIND],
    &[
        0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
    ],
);

static DELAY: Gauge<0> = Gauge::new(
    "platform.job_delay",
    "s",
    "How long the longest-waiting runnable job has waited past its run_at, 0 when none waits, \
     recorded by each replica every 15 seconds.",
    [],
);

/// The payload of a queued job, which a module declares for each kind of job it enqueues. A
/// payload type changes only by adding fields that may be absent, so a replica of the previous
/// release still runs a job that the new release enqueued; any other change is a new kind.
pub trait Job: Serialize + DeserializeOwned + Send + 'static {
    /// The kind, `<module>.<name>`, such as `mail.send`.
    const KIND: &'static str;
    /// The most attempts its runs make before it moves to the dead-letter state (OPS-10).
    const MOST_ATTEMPTS: u32 = 10;
    /// How long one run may take before it is cancelled, and fails (PLATFORM-062).
    const TIME_LIMIT: Duration = Duration::from_secs(300);
}

/// The user or workspace a job runs for, whose data its handler's queries filter by
/// (PLATFORM-012). Only a proof builds one that names a user or a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobScope {
    user: Option<UserId>,
    workspace: Option<WorkspaceId>,
}

impl JobScope {
    /// The scope of a job that runs for no one, such as scheduled maintenance's.
    pub fn none() -> Self {
        Self {
            user: None,
            workspace: None,
        }
    }

    /// The scope that a proof gives, which only the proofs of `platform` build.
    pub(crate) fn proven(user: Option<UserId>, workspace: Option<WorkspaceId>) -> Self {
        Self { user, workspace }
    }

    /// The user the job runs for, if any.
    pub fn user(&self) -> Option<UserId> {
        self.user
    }

    /// The workspace the job runs for, if any.
    pub fn workspace(&self) -> Option<WorkspaceId> {
        self.workspace
    }
}

/// A run of a job, which only the dispatcher and the scheduler build, so only a run opens a unit
/// of work through [`Database::for_job`].
#[derive(Clone, Debug)]
pub struct JobContext {
    id: Uuid,
    attempt: u32,
    scope: JobScope,
}

impl JobContext {
    /// The queued job's ID, or the claim of a periodic job's run.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Which attempt of its job this run is, from 1.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The scope the job runs for, which is also the scope of any job the run enqueues.
    pub fn scope(&self) -> JobScope {
        self.scope
    }
}

/// Why a run failed.
#[derive(Debug, thiserror::Error)]
pub enum JobFailure {
    /// A failure that another attempt may not meet, such as a server that didn't answer: the job
    /// runs again after a backoff, until its last attempt.
    #[error(transparent)]
    Transient(Internal),
    /// A failure that every attempt would meet, such as a recipient that the mail server
    /// refuses: the job moves to the dead-letter state at once (PLATFORM-013).
    #[error(transparent)]
    Permanent(Internal),
}

impl From<Internal> for JobFailure {
    fn from(error: Internal) -> Self {
        Self::Transient(error)
    }
}

/// A job's kind, or a periodic job's name, that is registered twice.
#[derive(Debug, thiserror::Error)]
#[error("the job {0} is registered twice")]
pub struct Duplicate(&'static str);

type QueuedRun = Arc<
    dyn Fn(JobContext, serde_json::Value) -> BoxFuture<'static, Result<(), JobFailure>>
        + Send
        + Sync,
>;

type PeriodicRun =
    Arc<dyn Fn(JobContext) -> BoxFuture<'static, Result<(), Internal>> + Send + Sync>;

/// A kind of queued job, with its handler.
#[derive(Clone)]
struct Queued {
    kind: &'static str,
    most_attempts: u32,
    time_limit: Duration,
    run: QueuedRun,
}

/// A periodic job, with its handler.
#[derive(Clone)]
struct Periodic {
    name: &'static str,
    interval: Duration,
    time_limit: Duration,
    run: PeriodicRun,
}

/// Every job's handler, in one registry (D13), which the composition root fills and `serve`
/// runs.
pub struct Jobs {
    queued: HashMap<&'static str, Queued>,
    periodic: HashMap<&'static str, Periodic>,
}

impl Jobs {
    /// The registry, holding `platform`'s sweeps of its own tables on `database`, which delete
    /// the dead letters older than `retention` (PLATFORM-064).
    pub fn new(database: &Database, retention: Duration) -> Self {
        let mut jobs = Self {
            queued: HashMap::new(),
            periodic: HashMap::new(),
        };
        sweeps::register(&mut jobs, database, retention);
        jobs
    }

    /// Registers `handler` for the jobs of kind `J`. A payload that doesn't read as a `J` fails
    /// at once, since every attempt would read it the same way.
    pub fn queued<J, F, Fut>(&mut self, handler: F) -> Result<(), Duplicate>
    where
        J: Job,
        F: Fn(JobContext, J) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), JobFailure>> + Send + 'static,
    {
        if self.queued.contains_key(J::KIND) {
            return Err(Duplicate(J::KIND));
        }
        let handler = Arc::new(handler);
        let run: QueuedRun = Arc::new(move |context, payload| {
            let handler = handler.clone();
            Box::pin(async move {
                // The decoding error's text may quote the payload, so the failure names none.
                let job: J = serde_json::from_value(payload).map_err(|_| {
                    JobFailure::Permanent(Internal::new("the payload doesn't read as its kind"))
                })?;
                handler(context, job).await
            })
        });
        self.queued.insert(
            J::KIND,
            Queued {
                kind: J::KIND,
                most_attempts: J::MOST_ATTEMPTS,
                time_limit: J::TIME_LIMIT,
                run,
            },
        );
        Ok(())
    }

    /// Registers `handler` as the periodic job `name`, which runs every `interval`, each run
    /// for at most `time_limit` (OPS-11).
    pub fn periodic<F, Fut>(
        &mut self,
        name: &'static str,
        interval: Duration,
        time_limit: Duration,
        handler: F,
    ) -> Result<(), Duplicate>
    where
        F: Fn(JobContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), Internal>> + Send + 'static,
    {
        if self.periodic.contains_key(name) {
            return Err(Duplicate(name));
        }
        let handler = Arc::new(handler);
        let run: PeriodicRun = Arc::new(move |context| Box::pin(handler(context)));
        self.periodic.insert(
            name,
            Periodic {
                name,
                interval,
                time_limit,
                run,
            },
        );
        Ok(())
    }

    /// Runs the dispatcher and the scheduler on `database` until `stopping` turns true, then
    /// the runs in flight until `grace` has passed, and then cancels and releases those still
    /// going (PLATFORM-063). `serve` gives it the time it gives the requests in flight.
    pub async fn run(
        self,
        database: Database,
        clock: SharedClock,
        stopping: watch::Receiver<bool>,
        grace: Duration,
    ) {
        let delay = tokio::spawn(watch_delay(
            database.clone(),
            clock.clone(),
            stopping.clone(),
        ));
        let deadline = deadline(clock.clone(), stopping.clone(), grace);
        let queued: Arc<HashMap<_, _>> = Arc::new(self.queued);
        let periodic: Arc<HashMap<_, _>> = Arc::new(self.periodic);
        tokio::join!(
            dispatcher::dispatch(
                queued,
                database.clone(),
                clock.clone(),
                stopping.clone(),
                deadline.clone(),
            ),
            scheduler::schedule(periodic, database, clock, stopping, deadline),
        );
        delay.abort();
    }

    /// Runs every queued job that is runnable now, one at a time, until none is, and returns how
    /// many runs it made: a test's way to run the jobs its requests enqueued, without waiting
    /// for a dispatcher's poll.
    #[cfg(feature = "test-support")]
    pub async fn run_pending(&self, database: &Database, clock: &SharedClock) -> usize {
        let kinds: Vec<String> = self.queued.keys().map(|kind| (*kind).to_owned()).collect();
        let mut runs = 0;
        loop {
            let claimed = match database.claim_jobs(&kinds, 1, LEASE).await {
                Ok(claimed) => claimed,
                Err(error) => {
                    tracing::warn!(
                        { names::EXCEPTION_MESSAGE } = crate::errors::chain(&error),
                        "can't claim jobs"
                    );
                    return runs;
                }
            };
            let Some(job) = claimed.into_iter().next() else {
                return runs;
            };
            let Some(queued) = self.queued.get(job.kind.as_str()).cloned() else {
                return runs;
            };
            dispatcher::run_job(queued, job, database.clone(), clock.clone()).await;
            runs += 1;
        }
    }

    /// Runs the periodic job `name` once, now, outside its schedule: a test's way to prove what
    /// a periodic job does, apart from when the scheduler runs it.
    #[cfg(feature = "test-support")]
    pub async fn run_periodic(&self, name: &str) -> Result<(), Internal> {
        let job = self
            .periodic
            .get(name)
            .ok_or_else(|| Internal::new("no periodic job has the name"))?;
        let context = JobContext {
            id: Uuid::nil(),
            attempt: 1,
            scope: JobScope::none(),
        };
        (job.run)(context).await
    }
}

impl UnitOfWork {
    /// Enqueues `job` to run for `scope` once the unit of work commits, and never if it rolls
    /// back (PLATFORM-011), with the trace context of the current span, which its run links to
    /// (OBS-07).
    pub async fn enqueue<J: Job>(&mut self, scope: JobScope, job: &J) -> Result<(), Internal> {
        let payload = serde_json::to_value(job).map_err(Internal::new)?;
        let traceparent = telemetry::traceparent(&tracing::Span::current());
        sqlx::query!(
            "INSERT INTO jobs (kind, payload, user_id, workspace_id, traceparent)
            VALUES ($1, $2, $3, $4, $5)",
            J::KIND,
            payload,
            scope.user.map(UserId::uuid),
            scope.workspace.map(WorkspaceId::uuid),
            traceparent,
        )
        .execute(self.executor())
        .await?;
        Ok(())
    }
}

/// How a run ended (PLATFORM-065).
#[derive(Clone, Copy, Debug)]
enum Outcome {
    Succeeded,
    Retried,
    DeadLettered,
    Failed,
    Released,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Retried => "retried",
            Self::DeadLettered => "dead_lettered",
            Self::Failed => "failed",
            Self::Released => "released",
        }
    }
}

/// Counts a run of the job `kind` that ended as `outcome`.
fn count(kind: &'static str, outcome: Outcome) {
    RUNS.add(1, [kind.into(), outcome.label().into()]);
}

/// Renews the lease of a run every [`RENEWAL`], for as long as the run lasts. A renewal that
/// fails is logged, and the next one tries again; the lease outlasts two that fail.
async fn renew_every<F, Fut>(clock: &SharedClock, renew: F) -> std::convert::Infallible
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<(), Internal>>,
{
    loop {
        clock.sleep(RENEWAL).await;
        if let Err(error) = renew().await {
            tracing::warn!(
                { names::EXCEPTION_MESSAGE } = crate::errors::chain(&error),
                "can't renew a job's lease"
            );
        }
    }
}

/// The one deadline of the runs in flight: it passes `grace` after `stopping` turns true, or
/// after its sender drops, for the dispatcher and the scheduler alike.
fn deadline(
    clock: SharedClock,
    mut stopping: watch::Receiver<bool>,
    grace: Duration,
) -> watch::Receiver<bool> {
    let (passed, deadline) = watch::channel(false);
    tokio::spawn(async move {
        // Whether the signal arrived or the sender dropped, the jobs stop, and the grace starts.
        stopping
            .wait_for(|stopping| *stopping)
            .await
            .map(drop)
            .unwrap_or_default();
        clock.sleep(grace).await;
        passed.send_replace(true);
    });
    deadline
}

/// Records how long the longest-waiting runnable job has waited, every [`DELAY_EVERY`], until
/// `stopping` turns true.
async fn watch_delay(database: Database, clock: SharedClock, mut stopping: watch::Receiver<bool>) {
    while !*stopping.borrow() {
        match database.job_delay().await {
            Ok(delay) => DELAY.set(delay.as_secs(), []),
            Err(error) => tracing::warn!(
                { names::EXCEPTION_MESSAGE } = crate::errors::chain(&error),
                "can't read how long jobs wait"
            ),
        }
        tokio::select! {
            () = clock.sleep(DELAY_EVERY) => {}
            changed = stopping.changed() => if changed.is_err() { return },
        }
    }
}
