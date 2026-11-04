//! CPU-heavy work (CODE-05, OBS-06, PLATFORM-078).

use std::{
    sync::mpsc::{self, Sender},
    thread,
    time::Duration,
};

use platform::{
    blocking::Workers,
    clock::{Clock, SystemClock},
    telemetry::{Service, testing},
};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
};
use tracing::Instrument;

/// How long a test waits for a work that should start, and for one that shouldn't.
const WAIT: Duration = Duration::from_secs(5);
const ABSENT: Duration = Duration::from_millis(300);

#[tokio::test]
async fn obs_06_cpu_heavy_work_runs_on_a_blocking_thread_in_a_span_of_its_own() {
    testing::install(Service {
        name: "api",
        version: "0.0.0",
    })
    .unwrap();
    let caller = std::thread::current().id();
    let request = tracing::info_span!("request");

    let worker = Workers::new()
        .run("work", || {
            tracing::info!("the blocking work ran");
            std::thread::current().id()
        })
        .instrument(request.clone())
        .await
        .unwrap();
    request.in_scope(|| tracing::info!("the caller ran"));

    assert_ne!(
        worker, caller,
        "the work ran on a thread of its own (CODE-05)"
    );
    let lines = testing::all_logged();
    let line = |message: &str| {
        let found: Vec<_> = lines.iter().filter(|l| l["message"] == message).collect();
        assert_eq!(found.len(), 1, "{message}");
        found[0].clone()
    };
    let (work, called) = (line("the blocking work ran"), line("the caller ran"));
    assert_eq!(
        work["trace_id"], called["trace_id"],
        "the work is in the caller's trace"
    );
    assert_ne!(
        work["span_id"], called["span_id"],
        "the work has a span of its own"
    );
}

#[tokio::test]
async fn platform_078_cpu_heavy_work_runs_at_most_one_at_once_per_processor() {
    let workers = Workers::new();
    assert_eq!(
        workers.turns(),
        thread::available_parallelism().unwrap().get(),
        "one turn per processor the tests may use"
    );
    let mut gate = Gate::new();
    let mut running = gate.take_every_turn(&workers).await;

    let waiting = gate.start(&workers);
    assert_eq!(
        gate.started_within(ABSENT).await,
        None,
        "a work waits while every turn is taken"
    );
    gate.end(0);
    running.swap_remove(0).await.unwrap();
    assert_eq!(
        gate.started_within(WAIT).await,
        Some(workers.turns()),
        "the waiting work starts once a turn is free"
    );
    gate.end_all();
    waiting.await.unwrap();
}

#[tokio::test]
async fn platform_078_a_work_keeps_its_turn_until_it_ends_and_a_caller_that_stops_waiting_gives_up_its_place()
 {
    let workers = Workers::new();
    let mut gate = Gate::new();
    let running = gate.take_every_turn(&workers).await;
    let gave_up = gate.start(&workers);
    let waiting = gate.start(&workers);
    SystemClock.sleep(ABSENT).await;

    // The callers of the running works and of the first waiting one stop waiting, as a request
    // past its timeout does.
    for caller in running.iter().chain([&gave_up]) {
        caller.abort();
    }
    assert_eq!(
        gate.started_within(ABSENT).await,
        None,
        "the works whose callers stopped waiting keep their turns"
    );
    gate.end(0);
    assert_eq!(
        gate.started_within(WAIT).await,
        Some(workers.turns() + 1),
        "the turn goes to the next caller still waiting"
    );
    gate.end_all();
    waiting.await.unwrap();
    assert_eq!(
        gate.started_within(ABSENT).await,
        None,
        "the work of the caller that stopped waiting never ran"
    );
}

/// Works, numbered in the order they are started, that report when they run and run until the
/// test ends them.
struct Gate {
    runs: (UnboundedSender<usize>, UnboundedReceiver<usize>),
    ends: Vec<Option<Sender<()>>>,
}

impl Gate {
    fn new() -> Self {
        Self {
            runs: unbounded_channel(),
            ends: Vec::new(),
        }
    }

    /// Runs the next work through `workers`, from a task of its own.
    fn start(&mut self, workers: &Workers) -> JoinHandle<()> {
        let work = self.ends.len();
        let (end, ended) = mpsc::channel::<()>();
        self.ends.push(Some(end));
        let runs = self.runs.0.clone();
        let workers = workers.clone();
        tokio::spawn(async move {
            workers
                .run("work", move || {
                    runs.send(work).unwrap();
                    // The test ends the work by dropping the sender, which ends the wait.
                    ended.recv().unwrap_err();
                })
                .await
                .unwrap();
        })
    }

    /// Starts a work for each turn of `workers`, and waits until they all run.
    async fn take_every_turn(&mut self, workers: &Workers) -> Vec<JoinHandle<()>> {
        let callers: Vec<_> = (0..workers.turns()).map(|_| self.start(workers)).collect();
        for _ in &callers {
            assert!(
                self.started_within(WAIT).await.is_some(),
                "a work runs while a turn is free"
            );
        }
        callers
    }

    /// The work that ran next, if one did within `wait`.
    async fn started_within(&mut self, wait: Duration) -> Option<usize> {
        tokio::select! {
            work = self.runs.1.recv() => Some(work.expect("the gate holds a sender")),
            () = SystemClock.sleep(wait) => None,
        }
    }

    /// Ends the work numbered `work`.
    fn end(&mut self, work: usize) {
        self.ends[work].take();
    }

    /// Ends every work, those that haven't run included, which end as soon as they run.
    fn end_all(&mut self) {
        self.ends.clear();
    }
}
