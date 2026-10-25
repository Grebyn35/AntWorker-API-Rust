//! The trace-ID guarantee (D10, OBS-08), through the image and the observability stack that
//! Compose runs: a trace ID that a response returns leads to the request's logs, and, for a
//! request that ended in an error, to its trace.

use std::process::Output;

use serde_json::Value;

use crate::{
    docker::{compose, lossy, postgres_network},
    image::{Image, PORT, settings},
    support::{self, TestDatabase, http_request},
};

/// Waits until `url`, on Compose's network, answers with a body that holds `wanted`, and returns
/// the body. The wait runs in a shell of the stack, from the image of Prometheus, since the test
/// itself never sleeps (TEST-07); the stack takes seconds to store what it receives, and the
/// collector waits 15 seconds before it decides to keep a trace.
fn fetch_once_it_holds(url: &str, wanted: &str) -> Output {
    let script = format!(
        "timeout 90 sh -c 'until wget -qO /tmp/body \"$0\" && grep -q \"$1\" /tmp/body; do sleep 1; done' \
         \"{url}\" \"{wanted}\" && cat /tmp/body"
    );
    std::process::Command::new("docker")
        .args([
            "compose",
            "--profile",
            "observability",
            "run",
            "--rm",
            "--no-deps",
            "--no-TTY",
            "--entrypoint",
            "sh",
            "prometheus",
            "-c",
            &script,
        ])
        .output()
        .expect("run docker compose")
}

#[tokio::test]
async fn obs_08_the_trace_id_of_a_4xx_response_leads_to_its_logs_and_its_trace() {
    compose(&[
        "--profile",
        "observability",
        "up",
        "--detach",
        "--wait",
        "collector",
        "loki",
        "tempo",
        "prometheus",
    ]);
    let image = Image::pushed();
    let database = TestDatabase::empty().await;
    let network = postgres_network();
    let mut settings = settings(&database);
    settings.push("OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4318".to_owned());
    let bootstrap = database
        .with_roles_locked(async { image.run(&network, &settings, "bootstrap") })
        .await;
    assert!(bootstrap.status.success(), "{}", lossy(&bootstrap));
    let migrate = image.run(&network, &settings, "migrate");
    assert!(migrate.status.success(), "{}", lossy(&migrate));
    let server = image.serve(&network, &settings);
    assert_eq!(server.health(), "healthy", "{}", server.logs());

    let response = http_request(server.address(PORT), "GET", "/v1/no-such-resource", &[])
        .await
        .expect("an answer");
    let body: Value = serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap_or_default())
        .expect("a problem");
    let trace = body["trace_id"].as_str().expect("the trace ID").to_owned();
    let logs = fetch_once_it_holds(
        &format!(
            "http://loki:3100/loki/api/v1/query_range?query=%7Bservice_name%3D%22{}%22%7D%20%7C%20trace_id%3D%22{trace}%22",
            support::service_name()
        ),
        "request completed",
    );
    let stored = fetch_once_it_holds(&format!("http://tempo:3200/api/traces/{trace}"), "404");

    assert!(response.starts_with("HTTP/1.1 404 "), "{response}");
    assert!(logs.status.success(), "{}", lossy(&logs));
    assert!(lossy(&logs).contains(&trace), "{}", lossy(&logs));
    assert!(stored.status.success(), "{}", lossy(&stored));
    assert!(
        lossy(&stored).contains("http.response.status_code"),
        "{}",
        lossy(&stored)
    );
}
