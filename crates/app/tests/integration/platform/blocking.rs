//! CPU-heavy work (CODE-05, OBS-06, PLATFORM-060).

use std::sync::mpsc::{self, Sender};

use platform::{
    blocking::Workers,
    telemetry::{Service, testing},
};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task::JoinHandle,
};
use tracing::Instrument;

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
async fn platform_060_a_work_waits_while_every_turn_is_taken_and_starts_once_one_is_free() {
    let workers = Workers::new();
    let mut gate = Gate::new();
    let running = gate.take_every_turn(&workers).await;

    let waiting = gate.start(&workers);
    assert_eq!(
        gate.waits_or_starts(&workers, 1).await,
        None,
        "a work waits while every turn is taken"
    );
    gate.end(0);
    assert_eq!(
        gate.next_started().await,
        workers.turns(),
        "the waiting work starts once a turn is free"
    );
    gate.end_all();
    for caller in running {
        caller.await.unwrap();
    }
    waiting.await.unwrap();
}

#[tokio::test]
async fn platform_060_a_work_keeps_its_turn_until_it_ends_and_a_caller_that_stops_waiting_gives_up_its_place()
 {
    let workers = Workers::new();
    let mut gate = Gate::new();
    let running = gate.take_every_turn(&workers).await;
    let gave_up = gate.start(&workers);
    let waiting = gate.start(&workers);
    assert_eq!(gate.waits_or_starts(&workers, 2).await, None);

    // The callers of the running works and of the first waiting one stop waiting, as a request
    // past its timeout does.
    for caller in running.iter().chain([&gave_up]) {
        caller.abort();
    }
    for caller in running.into_iter().chain([gave_up]) {
        assert!(caller.await.unwrap_err().is_cancelled());
    }
    assert_eq!(
        gate.waits_or_starts(&workers, 1).await,
        None,
        "the works whose callers stopped waiting keep their turns"
    );
    gate.end(0);
    assert_eq!(
        gate.next_started().await,
        workers.turns() + 1,
        "the turn goes to the next caller still waiting"
    );
    gate.end_all();
    waiting.await.unwrap();
    assert_eq!(
        gate.started_yet(),
        None,
        "the work of the caller that stopped waiting never ran"
    );
}

#[tokio::test]
async fn platform_060_a_turn_taken_before_its_works_runs_each_and_goes_once_it_is_dropped() {
    let workers = Workers::new();
    let mut turns = Vec::new();
    for _ in 0..workers.turns() {
        turns.push(workers.turn().await.unwrap());
    }
    let mut gate = Gate::new();
    let waiting = gate.start(&workers);
    assert_eq!(
        gate.waits_or_starts(&workers, 1).await,
        None,
        "a work waits while callers hold every turn"
    );

    // A holder runs its works in its turn, one after another, each giving the turn back.
    let turn = turns.remove(0);
    let (first, turn) = turn.run("work", || 1).await.unwrap();
    let (second, turn) = turn.run("work", || 2).await.unwrap();

    assert_eq!((first, second), (1, 2));
    assert_eq!(gate.started_yet(), None, "the other work still waits");
    drop(turn);
    assert_eq!(
        gate.next_started().await,
        0,
        "the waiting work starts once a holder drops its turn"
    );
    gate.end_all();
    waiting.await.unwrap();
}

#[tokio::test]
async fn platform_060_a_work_keeps_the_turn_that_its_holder_dropped_until_it_ends() {
    let workers = Workers::new();
    let mut turns = Vec::new();
    for _ in 0..workers.turns() {
        turns.push(workers.turn().await.unwrap());
    }
    let turn = turns.remove(0);
    let (started, mut running) = unbounded_channel();
    let (end, ended) = mpsc::channel::<()>();
    // A work in the turn, whose caller stops waiting once it runs, as a request past its
    // timeout does, which drops the turn with it.
    let caller = tokio::spawn(async move {
        turn.run("work", move || {
            started.send(()).unwrap();
            ended.recv().unwrap_err();
        })
        .await
        .unwrap();
    });
    running.recv().await.unwrap();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    let mut gate = Gate::new();
    let waiting = gate.start(&workers);

    assert_eq!(
        gate.waits_or_starts(&workers, 1).await,
        None,
        "the work keeps the turn that its holder dropped"
    );
    drop(end);
    assert_eq!(
        gate.next_started().await,
        0,
        "the turn goes once the work has ended"
    );
    gate.end_all();
    waiting.await.unwrap();
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
            self.next_started().await;
        }
        callers
    }

    /// Waits until `count` callers of `workers` wait for a turn while no turn is free, and returns
    /// `None`, or until a work starts, and returns it. A turn that comes free while callers wait
    /// goes to one of them, whose work then starts, so the wait ends either way.
    async fn waits_or_starts(&mut self, workers: &Workers, count: usize) -> Option<usize> {
        loop {
            if let Some(work) = self.started_yet() {
                return Some(work);
            }
            if workers.waiting() == count && workers.free() == 0 {
                return None;
            }
            tokio::task::yield_now().await;
        }
    }

    /// The work that runs next, once it runs.
    async fn next_started(&mut self) -> usize {
        self.runs.1.recv().await.expect("the gate holds a sender")
    }

    /// The work that ran since the last one the test read, if one did.
    fn started_yet(&mut self) -> Option<usize> {
        self.runs.1.try_recv().ok()
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
