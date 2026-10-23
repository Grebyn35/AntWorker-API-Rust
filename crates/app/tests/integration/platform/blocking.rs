//! CPU-heavy work (CODE-05, OBS-06).

use platform::telemetry::{Service, testing};
use tracing::Instrument;

#[tokio::test]
async fn obs_06_cpu_heavy_work_runs_on_a_blocking_thread_in_a_span_of_its_own() {
    testing::install(Service {
        name: "api",
        version: "0.0.0",
    });
    let caller = std::thread::current().id();
    let request = tracing::info_span!("request");

    let worker = platform::blocking::run("work", || {
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
