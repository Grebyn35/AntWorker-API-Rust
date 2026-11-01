//! Background jobs (the platform spec's Jobs): enqueueing, the dispatcher, the scheduler and the
//! sweeps, with probe jobs whose runs the tests steer, on a test clock.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use platform::{
    authn::Principal,
    clock::{SharedClock, TestClock},
    db::Database,
    errors::Internal,
    jobs::{Job, JobContext, JobFailure, JobScope, Jobs},
    telemetry::{
        Service, names,
        testing::{
            self,
            collector::{Collector, attribute},
        },
    },
    types::{SessionId, UserId},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    sync::{Semaphore, mpsc, watch},
    task::JoinHandle,
};

use crate::support::{self, Api, Options, TestDatabase, mailbox};

/// How long a shutdown lets runs go on, in these tests.
const GRACE: Duration = Duration::from_secs(5);

/// The dispatcher's poll, the scheduler's tick, and a lease's renewal (the platform spec's
/// Jobs).
const POLL: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_secs(10);
const RENEWAL: Duration = Duration::from_secs(20);

/// A job whose runs a test steers.
#[derive(Serialize, Deserialize)]
struct Probe {
    label: String,
}

impl Job for Probe {
    const KIND: &'static str = "tests.probe";
    const MOST_ATTEMPTS: u32 = 3;
    const TIME_LIMIT: Duration = Duration::from_secs(600);
}

/// A job whose runs never end on their own, so its time limit ends them.
#[derive(Serialize, Deserialize)]
struct Hanging;

impl Job for Hanging {
    const KIND: &'static str = "tests.hanging";
    const TIME_LIMIT: Duration = Duration::from_secs(30);
}

/// What a probe's run does.
#[derive(Clone, Copy, Debug)]
enum Act {
    Succeed,
    Fail,
    FailForGood,
    /// Waits until the test releases it, then succeeds.
    Wait,
}

/// The test's side of a registry's probes: what each run does, in order, then succeeds; what
/// each run saw; and the label of each run that started.
struct Steering {
    acts: Mutex<VecDeque<Act>>,
    contexts: Mutex<Vec<JobContext>>,
    started: mpsc::UnboundedSender<String>,
    release: Semaphore,
}

impl Steering {
    fn new(acts: &[Act]) -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        let (started, starts) = mpsc::unbounded_channel();
        let steering = Arc::new(Self {
            acts: Mutex::new(acts.iter().copied().collect()),
            contexts: Mutex::new(Vec::new()),
            started,
            release: Semaphore::new(0),
        });
        (steering, starts)
    }

    async fn run(&self, context: JobContext, label: String) -> Result<(), JobFailure> {
        let act = self
            .acts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Act::Succeed);
        self.contexts.lock().unwrap().push(context);
        self.started.send(label).unwrap();
        match act {
            Act::Succeed => Ok(()),
            Act::Fail => Err(JobFailure::Transient(Internal::new("the probe failed"))),
            Act::FailForGood => Err(JobFailure::Permanent(Internal::new(
                "the probe failed for good",
            ))),
            Act::Wait => {
                self.release.acquire().await.unwrap().forget();
                Ok(())
            }
        }
    }
}

/// A registry whose probes `steering` steers, beside `platform`'s sweeps, and whose hanging
/// jobs never end.
fn registry(database: &Database, steering: &Arc<Steering>) -> Jobs {
    let mut jobs = Jobs::new(database, Duration::from_secs(14 * 86_400));
    let probes = steering.clone();
    jobs.queued(move |context, probe: Probe| {
        let steering = probes.clone();
        async move { steering.run(context, probe.label).await }
    })
    .unwrap();
    jobs.queued(|_, Hanging| std::future::pending()).unwrap();
    jobs
}

/// Enqueues `job` for `scope` in a unit of work of its own, which commits.
async fn enqueue<J: Job>(database: &Database, scope: JobScope, job: &J) {
    let mut uow = database.begin(&support::client()).await.unwrap();
    uow.enqueue(scope, job).await.unwrap();
    uow.commit().await.unwrap();
}

fn probe(label: &str) -> Probe {
    Probe {
        label: label.to_owned(),
    }
}

/// `jobs` running on `database` with `clock`, as `serve` runs them, until the sender stops them.
fn start(
    jobs: Jobs,
    database: &Database,
    clock: &Arc<TestClock>,
) -> (watch::Sender<bool>, JoinHandle<()>) {
    let (stop, stopping) = watch::channel(false);
    let clock: SharedClock = clock.clone();
    let running = tokio::spawn(jobs.run(database.clone(), clock, stopping, GRACE));
    (stop, running)
}

#[tokio::test]
async fn platform_011_a_job_exists_only_if_its_unit_of_work_commits() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let mut rolled_back = database.begin(&support::client()).await.unwrap();
    rolled_back
        .enqueue(JobScope::none(), &probe("rolled back"))
        .await
        .unwrap();

    drop(rolled_back);
    enqueue(&database, JobScope::none(), &probe("committed")).await;

    let jobs = db
        .owner_reads(
            "SELECT kind || ' ' || (payload->>'label') || ' ' || state::text || ' ' || attempts \
             FROM jobs",
        )
        .await;
    assert_eq!(jobs, ["tests.probe committed waiting 0"]);
}

#[tokio::test]
async fn platform_012_a_job_runs_with_the_scope_of_the_proof_that_enqueued_it() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, _starts) = Steering::new(&[]);
    let jobs = registry(&database, &steering);
    let user = UserId::from_uuid(uuid::Uuid::from_u128(7));
    #[expect(
        clippy::disallowed_methods,
        reason = "the test proves the scope that a principal gives a job"
    )]
    let principal = Principal::new(user, SessionId::from_uuid(uuid::Uuid::from_u128(8)));
    enqueue(&database, principal.job_scope(), &probe("mine")).await;

    let clock: SharedClock = Arc::new(TestClock::default());
    jobs.run_pending(&database, &clock).await;

    let contexts = steering.contexts.lock().unwrap();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].scope().user(), Some(user));
    assert_eq!(contexts[0].scope().workspace(), None);
    assert_eq!(contexts[0].attempt(), 1);
}

#[tokio::test]
async fn platform_013_a_failing_job_retries_with_backoff_then_moves_to_the_dead_letter_state() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, _starts) = Steering::new(&[Act::Fail, Act::Fail, Act::Fail]);
    let jobs = registry(&database, &steering);
    let clock: SharedClock = Arc::new(TestClock::default());
    enqueue(&database, JobScope::none(), &probe("flaky")).await;
    let state = "SELECT state::text || ' ' || attempts || ' ' || coalesce(failure, '-') || ' ' \
                 || (claim_id IS NULL)::text || ' ' || coalesce(payload->>'label', '-') FROM jobs";

    jobs.run_pending(&database, &clock).await;
    let first = db.owner_reads(state).await;
    let first_wait = db
        .owner_reads(
            "SELECT (run_at > now() + interval '4 seconds' \
             AND run_at <= now() + interval '10 seconds')::text FROM jobs",
        )
        .await;
    db.owner_executes("UPDATE jobs SET run_at = now()").await;
    jobs.run_pending(&database, &clock).await;
    let second_wait = db
        .owner_reads(
            "SELECT (run_at > now() + interval '9 seconds' \
             AND run_at <= now() + interval '20 seconds')::text FROM jobs",
        )
        .await;
    db.owner_executes("UPDATE jobs SET run_at = now()").await;
    jobs.run_pending(&database, &clock).await;

    assert_eq!(
        first,
        ["waiting 1 internal error: the probe failed true flaky"]
    );
    assert_eq!(first_wait, ["true"]);
    assert_eq!(second_wait, ["true"]);
    let dead = db.owner_reads(state).await;
    assert_eq!(dead, ["dead 3 internal error: the probe failed true flaky"]);
    let dead_at = db
        .owner_reads("SELECT (dead_at IS NOT NULL)::text FROM jobs")
        .await;
    assert_eq!(dead_at, ["true"]);
}

#[tokio::test]
async fn platform_013_a_permanent_failure_moves_the_job_to_the_dead_letter_state_at_once() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, _starts) = Steering::new(&[Act::FailForGood]);
    let jobs = registry(&database, &steering);
    enqueue(&database, JobScope::none(), &probe("doomed")).await;

    let clock: SharedClock = Arc::new(TestClock::default());
    jobs.run_pending(&database, &clock).await;

    let jobs = db
        .owner_reads("SELECT state::text || ' ' || attempts || ' ' || failure FROM jobs")
        .await;
    assert_eq!(jobs, ["dead 1 internal error: the probe failed for good"]);
}

#[tokio::test]
async fn platform_013_a_succeeded_job_keeps_no_row() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, _starts) = Steering::new(&[Act::Succeed]);
    let jobs = registry(&database, &steering);
    enqueue(&database, JobScope::none(), &probe("done")).await;

    let clock: SharedClock = Arc::new(TestClock::default());
    let runs = jobs.run_pending(&database, &clock).await;

    assert_eq!(runs, 1);
    let left = db.owner_reads("SELECT count(*)::text FROM jobs").await;
    assert_eq!(left, ["0"]);
}

/// A registry with the periodic job `tests.periodic`, every hour, whose runs count themselves,
/// say they started, and wait until the test releases them.
fn periodic_registry(
    database: &Database,
    runs: &Arc<AtomicU32>,
    started: &mpsc::UnboundedSender<()>,
    release: &Arc<Semaphore>,
) -> Jobs {
    let mut jobs = Jobs::new(database, Duration::from_secs(86_400));
    let (runs, started, release) = (runs.clone(), started.clone(), release.clone());
    jobs.periodic(
        "tests.periodic",
        Duration::from_secs(3_600),
        Duration::from_secs(3_600),
        move |_| {
            let (runs, started, release) = (runs.clone(), started.clone(), release.clone());
            async move {
                runs.fetch_add(1, Ordering::SeqCst);
                started.send(()).unwrap();
                release.acquire().await.unwrap().forget();
                Ok(())
            }
        },
    )
    .unwrap();
    jobs
}

#[tokio::test]
async fn platform_014_a_periodic_job_runs_once_per_interval_across_replicas_and_a_long_run_keeps_its_lease()
 {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let runs = Arc::new(AtomicU32::new(0));
    let (started, mut starts) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let (clock_a, clock_b) = (
        Arc::new(TestClock::default()),
        Arc::new(TestClock::default()),
    );
    let (stop_a, a) = start(
        periodic_registry(&database, &runs, &started, &release),
        &database,
        &clock_a,
    );
    starts.recv().await.unwrap();
    let lease = "SELECT lease_expires_at::text FROM job_schedules WHERE name = 'tests.periodic'";
    let first_lease = db.owner_reads(lease).await.remove(0);

    // A second replica looks while the first one's run holds the lease.
    let (stop_b, b) = start(
        periodic_registry(&database, &runs, &started, &release),
        &database,
        &clock_b,
    );
    clock_b.sleeping(TICK).await;
    // The first replica's run outlasts a renewal.
    clock_a.advance(RENEWAL);
    clock_a.sleeping(RENEWAL).await;
    let renewed = db
        .owner_reads(&format!(
            "SELECT (lease_expires_at > '{first_lease}'::timestamptz)::text FROM job_schedules \
             WHERE name = 'tests.periodic'"
        ))
        .await;
    release.add_permits(1);
    stop_a.send_replace(true);
    a.await.unwrap();
    // The second replica looks again once the run ended.
    clock_b.advance(TICK);
    clock_b.sleeping(TICK).await;
    stop_b.send_replace(true);
    b.await.unwrap();

    assert_eq!(renewed, ["true"]);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let next = db
        .owner_reads(
            "SELECT (next_run_at > now() + interval '59 minutes')::text || ' ' \
             || (claim_id IS NULL)::text FROM job_schedules WHERE name = 'tests.periodic'",
        )
        .await;
    assert_eq!(next, ["true true"]);
}

#[tokio::test]
async fn platform_061_a_replica_runs_at_most_4_jobs_at_once_and_claims_only_kinds_it_holds() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, mut starts) = Steering::new(&[Act::Wait; 5]);
    for label in ["1", "2", "3", "4", "5"] {
        enqueue(&database, JobScope::none(), &probe(label)).await;
    }
    db.owner_executes("INSERT INTO jobs (kind, payload) VALUES ('tests.unknown', '{}')")
        .await;
    let clock = Arc::new(TestClock::default());
    let (stop, running) = start(registry(&database, &steering), &database, &clock);

    for _ in 0..4 {
        starts.recv().await.unwrap();
    }
    clock.sleeping(POLL).await;
    let four_claimed = db
        .owner_reads("SELECT count(*)::text FROM jobs WHERE claim_id IS NOT NULL")
        .await;
    let fifth_waited = starts.try_recv().is_err();
    steering.release.add_permits(1);
    let fifth = starts.recv().await.unwrap();
    // A job committed while the replica is idle runs at its next poll.
    steering.release.add_permits(4);
    clock.sleeping(POLL).await;
    enqueue(&database, JobScope::none(), &probe("later")).await;
    clock.advance(POLL);
    let later = starts.recv().await.unwrap();
    stop.send_replace(true);
    running.await.unwrap();

    assert_eq!(four_claimed, ["4"]);
    assert!(fifth_waited);
    assert_eq!(fifth, "5");
    assert_eq!(later, "later");
    let unknown = db
        .owner_reads("SELECT attempts::text FROM jobs WHERE kind = 'tests.unknown'")
        .await;
    assert_eq!(unknown, ["0"]);
}

#[tokio::test]
async fn platform_062_a_run_that_passes_its_time_limit_is_cancelled_and_fails() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, _starts) = Steering::new(&[]);
    enqueue(&database, JobScope::none(), &Hanging).await;
    let clock = Arc::new(TestClock::default());
    let (stop, running) = start(registry(&database, &steering), &database, &clock);

    clock.sleeping(Hanging::TIME_LIMIT).await;
    clock.advance(Hanging::TIME_LIMIT);
    clock.sleeping(POLL).await;
    stop.send_replace(true);
    running.await.unwrap();

    let jobs = db
        .owner_reads(
            "SELECT state::text || ' ' || attempts || ' ' || failure || ' ' \
             || (claim_id IS NULL)::text FROM jobs",
        )
        .await;
    assert_eq!(
        jobs,
        ["waiting 1 internal error: the run passed its time limit true"]
    );
}

#[tokio::test]
async fn platform_062_a_job_whose_lease_ran_out_runs_again_and_the_stale_run_changes_nothing() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (stale, mut stale_starts) = Steering::new(&[Act::Wait]);
    let (fresh, _fresh_starts) = Steering::new(&[Act::Fail]);
    enqueue(&database, JobScope::none(), &probe("contested")).await;
    let clock = Arc::new(TestClock::default());
    let (stop, running) = start(registry(&database, &stale), &database, &clock);
    stale_starts.recv().await.unwrap();

    // As if the first replica stopped without ending or releasing the run.
    db.owner_executes("UPDATE jobs SET lease_expires_at = now() - interval '1 second'")
        .await;
    let other: SharedClock = Arc::new(TestClock::default());
    let rerun = registry(&database, &fresh)
        .run_pending(&database, &other)
        .await;
    stale.release.add_permits(1);
    stop.send_replace(true);
    running.await.unwrap();

    assert_eq!(rerun, 1);
    let jobs = db
        .owner_reads("SELECT state::text || ' ' || attempts || ' ' || failure FROM jobs")
        .await;
    assert_eq!(jobs, ["waiting 2 internal error: the probe failed"]);
}

#[tokio::test]
async fn platform_063_at_shutdown_nothing_more_is_claimed_and_runs_still_going_are_released() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, mut starts) = Steering::new(&[Act::Wait]);
    enqueue(&database, JobScope::none(), &probe("in flight")).await;
    let clock = Arc::new(TestClock::default());
    let (stop, running) = start(registry(&database, &steering), &database, &clock);
    starts.recv().await.unwrap();

    stop.send_replace(true);
    enqueue(&database, JobScope::none(), &probe("too late")).await;
    clock.sleeping(GRACE).await;
    clock.advance(GRACE);
    running.await.unwrap();

    assert!(starts.try_recv().is_err());
    let jobs = db
        .owner_reads(
            "SELECT (payload->>'label') || ' ' || attempts || ' ' || (claim_id IS NULL)::text \
             FROM jobs ORDER BY created_at, id",
        )
        .await;
    assert_eq!(jobs, ["in flight 0 true", "too late 0 true"]);
}

#[tokio::test]
async fn platform_064_the_sweeps_delete_what_nothing_reads_any_more() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, _starts) = Steering::new(&[]);
    let jobs = registry(&database, &steering);
    db.owner_executes(
        "INSERT INTO rate_limit_buckets (key_hash, count, window_started_at) \
         SELECT sha256(n::text::bytea), 1, now() - interval '25 hours' \
         FROM generate_series(1, 2500) AS n",
    )
    .await;
    db.owner_executes(
        "INSERT INTO rate_limit_buckets (key_hash, count, window_started_at) \
         VALUES ('\\x01', 1, now() - interval '23 hours')",
    )
    .await;
    let user = db
        .owner_reads(
            "INSERT INTO users (email, name) VALUES ('u@example.com', 'U') RETURNING id::text",
        )
        .await
        .remove(0);
    db.owner_executes(&format!(
        "INSERT INTO idempotency_records (user_id, key, fingerprint, created_at) VALUES \
         ('{user}', 'old', '\\x00', now() - interval '25 hours'), \
         ('{user}', 'kept', '\\x00', now() - interval '23 hours')"
    ))
    .await;
    db.owner_executes(
        "INSERT INTO jobs (kind, payload, state, dead_at) VALUES \
         ('tests.probe', '{\"label\": \"old\"}', 'dead', now() - interval '15 days'), \
         ('tests.probe', '{\"label\": \"kept\"}', 'dead', now() - interval '13 days')",
    )
    .await;
    db.owner_executes(
        "INSERT INTO jobs (kind, payload) VALUES ('tests.probe', '{\"label\": \"waiting\"}')",
    )
    .await;

    // Each sweep runs twice, as a job may (OPS-09): the second deletes nothing more.
    for _ in 0..2 {
        for sweep in [
            "platform.expire_rate_limit_buckets",
            "platform.expire_idempotency_records",
            "platform.expire_dead_letters",
        ] {
            jobs.run_periodic(sweep).await.unwrap();
        }
    }

    let buckets = db
        .owner_reads("SELECT encode(key_hash, 'hex') FROM rate_limit_buckets")
        .await;
    assert_eq!(buckets, ["01"]);
    let records = db.owner_reads("SELECT key FROM idempotency_records").await;
    assert_eq!(records, ["kept"]);
    let left = db
        .owner_reads("SELECT payload->>'label' FROM jobs ORDER BY created_at, id")
        .await;
    assert_eq!(left, ["kept", "waiting"]);
}

#[tokio::test]
async fn platform_013_a_dead_letter_keeps_its_payload_for_the_retention_serve_reads() {
    for (retention, kept) in [(None, vec!["13 days"]), (Some(1), vec![])] {
        let mut options = Options::default();
        if let Some(days) = retention {
            options = options.set("JOB_DEAD_LETTER_RETENTION_DAYS", days);
        }
        let api = Api::with(options).await;
        let db = api.database();
        for age in ["13 days", "15 days"] {
            db.owner_executes(&format!(
                "INSERT INTO jobs (kind, payload, state, dead_at) \
                 VALUES ('tests.probe', '{{\"label\": \"{age}\"}}', 'dead', now() - interval '{age}')"
            ))
            .await;
        }

        api.run_periodic("platform.expire_dead_letters").await;

        let left = db.owner_reads("SELECT payload->>'label' FROM jobs").await;
        assert_eq!(left, kept, "with a retention of {retention:?} days");
    }
}

#[tokio::test]
async fn platform_013_the_retention_takes_1_to_365_days() {
    for days in [0, 366] {
        let refused = crate::support::api::refusal(
            &Options::default().set("JOB_DEAD_LETTER_RETENTION_DAYS", days),
        );

        assert_eq!(
            refused,
            "JOB_DEAD_LETTER_RETENTION_DAYS must be a whole number from 1 to 365"
        );
    }
}

#[tokio::test]
async fn platform_062_a_job_whose_last_attempt_s_replica_stopped_moves_to_the_dead_letter_state() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, _starts) = Steering::new(&[]);
    // A job at its last attempt, whose replica stopped during it: its lease ran out.
    db.owner_executes(&format!(
        "INSERT INTO jobs (kind, payload, attempts, claim_id, lease_expires_at) \
         VALUES ('tests.probe', '{{\"label\": \"lost\"}}', {}, gen_random_uuid(), \
         now() - interval '1 second')",
        Probe::MOST_ATTEMPTS
    ))
    .await;

    let clock: SharedClock = Arc::new(TestClock::default());
    registry(&database, &steering)
        .run_pending(&database, &clock)
        .await;

    assert!(steering.contexts.lock().unwrap().is_empty());
    let jobs = db
        .owner_reads("SELECT state::text || ' ' || attempts || ' ' || failure FROM jobs")
        .await;
    assert_eq!(
        jobs,
        ["dead 4 the replica of its last attempt stopped during it"]
    );
}

#[tokio::test]
async fn platform_063_a_run_in_flight_goes_on_after_the_signal_until_it_ends() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let (steering, mut starts) = Steering::new(&[Act::Wait]);
    enqueue(&database, JobScope::none(), &probe("in flight")).await;
    let clock = Arc::new(TestClock::default());
    let (stop, running) = start(registry(&database, &steering), &database, &clock);
    starts.recv().await.unwrap();

    stop.send_replace(true);
    clock.sleeping(GRACE).await;
    steering.release.add_permits(1);
    running.await.unwrap();

    let left = db.owner_reads("SELECT count(*)::text FROM jobs").await;
    assert_eq!(left, ["0"]);
}

/// A job that only the test of the runs' metric enqueues.
#[derive(Serialize, Deserialize)]
struct Counted;

impl Job for Counted {
    const KIND: &'static str = "tests.counted";
}

#[tokio::test]
async fn platform_065_each_run_is_counted_by_its_kind_and_outcome() {
    testing::install(Service {
        name: "api",
        version: "0.0.0",
    })
    .unwrap();
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let mut jobs = Jobs::new(&database, Duration::from_secs(86_400));
    jobs.queued(|_, Counted| async { Ok(()) }).unwrap();
    enqueue(&database, JobScope::none(), &Counted).await;

    let clock: SharedClock = Arc::new(TestClock::default());
    jobs.run_pending(&database, &clock).await;

    let counted: Vec<f64> = testing::recorded("platform.job_runs")
        .into_iter()
        .filter(|point| {
            point.labels.get(names::JOB_KIND).map(String::as_str) == Some("tests.counted")
        })
        .map(|point| {
            assert_eq!(point.labels[names::JOB_OUTCOME], "succeeded");
            point.value
        })
        .collect();
    assert_eq!(counted, [1.0]);
}

#[tokio::test]
async fn platform_065_a_runs_span_links_to_the_trace_that_enqueued_it() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let invitee = mailbox::address("linked");
    let trace = "4bf92f3577b34da6a3ce929d0e0e4736";
    let payload = json!({
        "message_id": "<linked@example.com>",
        "message": {
            "kind": "invitation",
            "to": invitee,
            "inviter": "Ada Lovelace",
            "workspace": "Acme",
            "role": "member",
            "expires_in_days": 7,
        },
    });
    database
        .owner_executes(&format!(
            "INSERT INTO jobs (kind, payload, traceparent) \
             VALUES ('mail.send', '{payload}', '00-{trace}-00f067aa0ba902b7-01')"
        ))
        .await;

    let _server = database
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint()),
        ])
        .await;
    let received = collector
        .wait_for(|received| {
            received
                .spans()
                .iter()
                .any(|(_, span)| span.name == "mail.send")
        })
        .await;

    let (_, span) = received
        .spans()
        .into_iter()
        .find(|(_, span)| span.name == "mail.send")
        .unwrap();
    assert_eq!(span.links.len(), 1);
    assert_eq!(hex(&span.links[0].trace_id), trace);
    assert_eq!(hex(&span.links[0].span_id), "00f067aa0ba902b7");
    assert_ne!(hex(&span.trace_id), trace);
    assert_eq!(
        attribute(&span.attributes, names::JOB_KIND).as_deref(),
        Some("mail.send")
    );
    assert_eq!(mailbox::to(&invitee).await.len(), 1);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
