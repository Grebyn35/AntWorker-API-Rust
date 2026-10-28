//! The trace-ID guarantee (D10, OBS-08), through the image and the observability stack that
//! Compose runs: a trace ID that a response returns leads to the request's logs, and, for a
//! request that ended in an error, to its trace. And the image's `serve`, which records its own
//! memory, as it exports it to the stack (PLATFORM-046).

use std::process::Output;

use serde_json::Value;

use crate::{
    docker::{Container, compose, docker, lossy, postgres_network},
    image::{Image, PORT, settings},
    support::{self, TestDatabase, http_request},
};

/// Waits until `url`, on Compose's network, answers with a body that holds `wanted`, and returns
/// the body. The wait runs in a shell of the stack, since the test itself never sleeps (TEST-07);
/// the stack takes seconds to store what it receives, and the collector waits 15 seconds before
/// it decides to keep a trace.
fn fetch_once_it_holds(url: &str, wanted: &str) -> Output {
    in_stack(&format!(
        "timeout 90 sh -c 'until wget -qO /tmp/body \"$0\" && grep -q \"$1\" /tmp/body; do sleep 1; done' \
         \"{url}\" \"{wanted}\" && cat /tmp/body"
    ))
}

/// Runs `script` in a shell on Compose's network, from the image of Prometheus.
fn in_stack(script: &str) -> Output {
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
            script,
        ])
        .output()
        .expect("run docker compose")
}

/// What says why a wait ended without what it waited for: when each service of the stack was
/// created and started, the collector's log, the last lines of Loki's and Tempo's, the API's
/// exports that finished, with the error of each that failed, as Prometheus holds them, and the
/// log of the API's instance.
fn why_nothing_arrived(server: &Container) -> String {
    let started = compose(&[
        "--profile",
        "observability",
        "ps",
        "--all",
        "--quiet",
        "collector",
        "loki",
        "tempo",
        "prometheus",
    ])
    .lines()
    .map(|id| {
        docker(&[
            "inspect",
            "--format",
            "{{.Name}} created {{.Created}}, started {{.State.StartedAt}}, restarted {{.RestartCount}} times",
            id,
        ])
    })
    .collect::<Vec<_>>()
    .join("\n");
    let collector = compose(&[
        "--profile",
        "observability",
        "logs",
        "--no-color",
        "--timestamps",
        "collector",
    ]);
    let stack = compose(&[
        "--profile",
        "observability",
        "logs",
        "--no-color",
        "--tail",
        "20",
        "loki",
        "tempo",
    ]);
    let exports = in_stack(
        "wget -qO- 'http://prometheus:9090/api/v1/query?query=%7B__name__%3D~%22otel_sdk_exporter_(span%7Clog)_exported_total%22%7D'",
    );
    format!(
        "{started}\n{collector}\n{stack}\nThe API's exports:\n{}\nThe API's instance:\n{}",
        lossy(&exports),
        server.logs()
    )
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
    assert!(
        logs.status.success(),
        "{}\n{}",
        lossy(&logs),
        why_nothing_arrived(&server)
    );
    assert!(lossy(&logs).contains(&trace), "{}", lossy(&logs));
    assert!(
        stored.status.success(),
        "{}\n{}",
        lossy(&stored),
        why_nothing_arrived(&server)
    );
    assert!(
        lossy(&stored).contains("http.response.status_code"),
        "{}",
        lossy(&stored)
    );
}

#[tokio::test]
async fn platform_046_serve_exports_its_resident_memory() {
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

    // The SDK exports the metrics every 15 seconds.
    let series = fetch_once_it_holds(
        &format!(
            "http://prometheus:9090/api/v1/query?query=process_memory_usage_bytes%7Bjob%3D%22{}%22%7D",
            support::service_name()
        ),
        "process_memory_usage_bytes",
    );

    assert!(
        series.status.success(),
        "{}\n{}",
        lossy(&series),
        why_nothing_arrived(&server)
    );
    assert!(lossy(&series).contains("\"value\""), "{}", lossy(&series));
}
