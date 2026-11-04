//! The logging setup (OBS-02, OBS-05), and the watch that prints a long test's lines (TEST-07).

use std::{
    io,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use platform::telemetry::{
    Service,
    testing::{self, Watch},
};

use crate::support::{self, TestDatabase};

const SERVICE: Service = Service {
    name: "api",
    version: "0.0.0",
};

#[test]
fn obs_05_events_from_other_crates_are_not_written() {
    testing::install(SERVICE).unwrap();

    tracing::info_span!("request").in_scope(|| {
        tracing::warn!(target: "sqlx::query", summary = "SELECT 1", "slow statement");
        tracing::warn!("the tests wrote this");
    });

    let lines = testing::all_logged();
    assert!(!lines.iter().any(|line| line["message"] == "slow statement"));
    assert!(
        lines
            .iter()
            .any(|line| line["message"] == "the tests wrote this")
    );
}

#[tokio::test]
async fn obs_02_a_json_line_carries_the_service_and_its_trace_context() {
    let database = TestDatabase::migrated().await;
    let mut server = database.serve(&[("ENVIRONMENT", "production")]).await;
    let name = support::service_name();

    let response = server
        .request(
            "GET",
            "/v1/capabilities",
            &[(
                "traceparent",
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            )],
        )
        .await;
    let completed = server
        .line(|line| line["message"] == "request completed")
        .await;

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    for line in [server.listening(), &completed] {
        assert_eq!(line["service.name"], name, "{line}");
        assert_eq!(line["service.version"], env!("CARGO_PKG_VERSION"), "{line}");
        assert_eq!(line["deployment.environment.name"], "production", "{line}");
        assert!(line["timestamp"].is_string(), "{line}");
    }
    assert!(
        server.listening().get("trace_id").is_none(),
        "a line outside any span carries no trace context"
    );
    assert_eq!(completed["trace_id"], "0af7651916cd43dd8448eb211c80319c");
    assert_eq!(completed["span_id"].as_str().unwrap().len(), 16);
}

/// What a watch printed.
#[derive(Clone, Default)]
struct Printed(Arc<Mutex<Vec<u8>>>);

impl Printed {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl io::Write for Printed {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_07_a_watch_prints_its_test_s_lines_then_each_line_it_logs_until_it_ends() {
    testing::install(SERVICE).unwrap();
    let test = std::thread::current().name().unwrap().to_owned();
    tracing::info!("logged before the watch printed");
    std::thread::spawn(|| tracing::info!("logged by another thread"))
        .join()
        .unwrap();
    let printed = Printed::default();

    let watch = Watch::printing_to(Duration::ZERO, printed.clone());
    while !printed.text().contains("logged before the watch printed") {
        std::thread::yield_now();
    }
    tracing::info!("logged while the watch prints");
    drop(watch);
    tracing::info!("logged after the watch ended");

    let text = printed.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[0].starts_with(&format!("{test} has run for 0 seconds")),
        "{text}"
    );
    for (line, message) in lines[1..].iter().zip([
        "logged before the watch printed",
        "logged while the watch prints",
    ]) {
        let logged = line.strip_prefix(&format!("{test}: ")).unwrap();
        let logged: serde_json::Value = serde_json::from_str(logged).unwrap();
        assert_eq!(logged["message"], message, "{text}");
    }
}
