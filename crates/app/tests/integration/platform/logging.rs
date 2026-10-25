//! The logging setup (OBS-02, OBS-05).

use platform::telemetry::{Service, testing};

use crate::support::{self, TestDatabase};

#[test]
fn obs_05_events_from_other_crates_are_not_written() {
    testing::install(Service {
        name: "api",
        version: "0.0.0",
    })
    .unwrap();

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
