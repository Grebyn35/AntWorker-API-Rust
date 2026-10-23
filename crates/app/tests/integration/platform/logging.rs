//! The logging setup of the real binary (OBS-02).

use std::path::Path;

use crate::support::TestDatabase;

#[tokio::test]
async fn obs_02_a_json_line_carries_the_service_and_its_trace_context() {
    let database = TestDatabase::migrated().await;
    let mut server = database.serve(&[("ENVIRONMENT", "production")]).await;
    let binary = Path::new(env!("CARGO_BIN_EXE_antworker-api"));
    let name = binary.file_stem().unwrap().to_str().unwrap();

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
