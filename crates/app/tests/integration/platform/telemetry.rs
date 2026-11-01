//! The export of telemetry over OTLP (PLATFORM-039, PLATFORM-042, OBS-10), and the metrics the
//! pipeline and the readiness check record (PLATFORM-040, PLATFORM-043, OBS-15).

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use http::{Method, StatusCode};
use opentelemetry_proto::tonic::{
    common::v1::any_value, resource::v1::Resource, trace::v1::span::SpanKind,
};
use platform::telemetry::testing::{
    self, Point,
    collector::{Collector, Received, attribute, text},
};

use crate::support::{Api, Options, TestDatabase, fixtures, service_name};

/// How often the instance checks its own database connection.
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// The requests that `points` recorded with exactly `labels`.
fn requests(points: &[Point], labels: &[(&str, &str)]) -> u64 {
    let wanted: BTreeMap<String, String> = labels
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    points
        .iter()
        .filter(|point| point.labels == wanted)
        .filter_map(|point| point.count)
        .sum()
}

/// The seconds that `points` recorded with exactly `labels`, all requests together.
fn seconds(points: &[Point], labels: &[(&str, &str)]) -> f64 {
    let wanted: BTreeMap<String, String> = labels
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    points
        .iter()
        .filter(|point| point.labels == wanted)
        .map(|point| point.value)
        .sum()
}

fn durations() -> Vec<Point> {
    testing::recorded("http.server.request.duration")
}

/// Asserts that `resource` names the service, the environment and a process.
fn assert_names_the_process(resource: &Resource, environment: &str) -> String {
    let attributes = &resource.attributes;
    assert_eq!(
        attribute(attributes, "service.name").as_deref(),
        Some(service_name())
    );
    assert_eq!(
        attribute(attributes, "service.version").as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        attribute(attributes, "deployment.environment.name").as_deref(),
        Some(environment)
    );
    let instance = attribute(attributes, "service.instance.id").unwrap();
    // A version 4 UUID: 36 characters, with the version in the 15th.
    assert_eq!(instance.len(), 36, "{instance}");
    assert_eq!(&instance[14..15], "4", "{instance}");
    assert_eq!(attributes.len(), 4, "{attributes:?}");
    instance
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test]
async fn platform_040_a_request_records_its_method_route_and_status() {
    let api = Api::new().await;
    let labels = [
        ("http.request.method", "GET"),
        ("http.route", "/v1/capabilities"),
        ("http.response.status_code", "200"),
    ];
    let before = requests(&durations(), &labels);

    let response = api.get("/v1/capabilities").send().await;

    assert_eq!(response.status, StatusCode::OK);
    assert!(requests(&durations(), &labels) > before);
}

#[tokio::test]
async fn platform_040_a_5xx_records_its_error_code_as_its_error_type() {
    let api = Api::with(Options::routes(fixtures::routes(Arc::default()))).await;
    let labels = [
        ("http.request.method", "GET"),
        ("http.route", "/v1/broken"),
        ("http.response.status_code", "500"),
        ("error.type", "request.internal_error"),
    ];
    let before = requests(&durations(), &labels);

    let response = api.get("/v1/broken").send().await;

    assert_eq!(response.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(requests(&durations(), &labels) > before);
}

#[tokio::test]
async fn platform_040_a_method_http_doesn_t_define_is_other() {
    let api = Api::new().await;
    let other = |points: &[Point]| -> u64 {
        points
            .iter()
            .filter(|point| {
                point.labels.get("http.request.method").map(String::as_str) == Some("_OTHER")
                    && point
                        .labels
                        .get("http.response.status_code")
                        .map(String::as_str)
                        == Some("405")
            })
            .filter_map(|point| point.count)
            .sum()
    };
    let before = other(&durations());

    let response = api
        .request(Method::from_bytes(b"PROPFIND").unwrap(), "/v1/capabilities")
        .send()
        .await;

    assert_eq!(response.status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(other(&durations()) > before);
    let traceparent = response.header("server-timing")["trace;desc=".len()..].to_owned();
    let trace = traceparent.split('-').nth(1).unwrap().to_owned();
    let lines = testing::logged(&trace);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(lines[0]["http.request.method"], "_OTHER");
}

#[tokio::test]
async fn platform_040_a_request_no_route_matched_records_no_route() {
    let api = Api::new().await;
    // Exactly these labels: no `http.route`, and no `error.type` on a 4xx.
    let labels = [
        ("http.request.method", "GET"),
        ("http.response.status_code", "404"),
    ];
    let before = requests(&durations(), &labels);

    let response = api.get("/v1/no-such-route").send().await;

    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert!(requests(&durations(), &labels) > before);
}

#[tokio::test]
async fn platform_040_a_request_records_how_long_it_took() {
    let gate = Arc::new(fixtures::Gate::default());
    let api = Api::with(
        Options::routes(fixtures::routes(gate.clone())).set("MAX_CONCURRENT_REQUESTS", 100),
    )
    .await;
    let clock = api.clock();
    let labels = [
        ("http.request.method", "GET"),
        ("http.route", "/v1/gate"),
        ("http.response.status_code", "204"),
    ];
    let before = seconds(&durations(), &labels);
    let entered = gate.entered.notified();
    let held = tokio::spawn({
        let api = api.clone();
        async move { api.get("/v1/gate").send().await.status }
    });
    entered.await;

    clock.advance(Duration::from_secs(2));
    gate.release.notify_one();

    assert_eq!(held.await.unwrap(), StatusCode::NO_CONTENT);
    let took = seconds(&durations(), &labels) - before;
    assert!(took >= 2.0, "{took}");
}

#[tokio::test]
async fn platform_040_patch_is_a_method_http_defines() {
    let api = Api::new().await;
    let labels = [
        ("http.request.method", "PATCH"),
        ("http.route", "/v1/capabilities"),
        ("http.response.status_code", "405"),
    ];
    let before = requests(&durations(), &labels);

    let response = api.request(Method::PATCH, "/v1/capabilities").send().await;

    assert_eq!(response.status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(requests(&durations(), &labels) > before);
}

#[tokio::test]
async fn obs_15_the_health_probes_record_no_request_metric() {
    let api = Api::new().await;

    for path in ["/health/live", "/health/ready"] {
        assert_eq!(api.get(path).send().await.status, StatusCode::OK);
    }

    let probes: Vec<Point> = durations()
        .into_iter()
        .filter(|point| {
            point
                .labels
                .get("http.route")
                .is_some_and(|route| route.starts_with("/health"))
        })
        .collect();
    assert!(probes.is_empty(), "{probes:#?}");
}

#[tokio::test]
async fn platform_046_the_resident_memory_is_recorded_at_once_and_every_15_seconds_after() {
    let api = Api::new().await;
    let clock = api.clock();
    // The kernel's report, which grows by a megabyte at each reading.
    let readings = Arc::new(AtomicU64::new(0));
    let counted = readings.clone();
    testing::watch_memory(clock.clone(), move || {
        let reading = counted.fetch_add(1, Ordering::SeqCst) + 1;
        async move { Some(reading * 1_000_000) }
    });
    let memory = || {
        testing::recorded("process.memory.usage")
            .iter()
            .map(|point| point.value)
            .next_back()
    };

    clock.sleeping(Duration::from_secs(15)).await;
    let first = memory();
    // Advancing the clock completes the sleep, and ends it: the watcher waits 15 seconds again
    // once it has recorded again.
    clock.advance(Duration::from_secs(15));
    clock.sleeping(Duration::from_secs(15)).await;
    let second = memory();

    assert_eq!(first, Some(1_000_000.0));
    assert_eq!(second, Some(2_000_000.0));
    assert_eq!(readings.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn platform_043_the_database_gauge_follows_each_check() {
    let database = TestDatabase::migrated().await;
    let api = Api::new().await;
    let clock = api.clock();
    let reachable = || {
        testing::recorded("platform.database_reachable")
            .into_iter()
            .find(|point| point.labels.get("db.namespace") == Some(&database.name().to_owned()))
            .map(|point| point.value)
    };
    database
        .app()
        .await
        .watch(api.readiness().clone(), clock.clone());
    clock.sleeping(CHECK_INTERVAL).await;
    let answered = reachable();

    database.cut_off_app().await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let unanswered = reachable();

    database
        .admin_executes(&format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO app",
            database.name()
        ))
        .await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let recovered = reachable();

    assert_eq!(answered, Some(1.0));
    assert_eq!(unanswered, Some(0.0));
    assert_eq!(recovered, Some(1.0));
}

#[tokio::test]
async fn platform_039_serve_exports_its_spans_and_log_records_with_their_trace_context() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let endpoint = collector.endpoint();
    let mut server = database
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint),
        ])
        .await;
    let trace = "0af7651916cd43dd8448eb211c80319c";

    let response = server
        .request(
            "GET",
            "/v1/capabilities",
            &[("traceparent", &format!("00-{trace}-b7ad6b7169203331-01"))],
        )
        .await;
    server
        .line(|line| line["message"] == "request completed")
        .await;
    let in_trace = |received: &Received| -> bool {
        received
            .spans()
            .iter()
            .any(|(_, span)| hex(&span.trace_id) == trace)
            && received.records().iter().any(|(_, record)| {
                hex(&record.trace_id) == trace
                    && record.body.as_ref().and_then(text).as_deref() == Some("request completed")
            })
    };
    let received = collector.wait_for(in_trace).await;

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let (resource, span) = received
        .spans()
        .into_iter()
        .find(|(_, span)| hex(&span.trace_id) == trace)
        .unwrap();
    assert_names_the_process(resource, "production");
    assert_eq!(span.name, "GET /v1/capabilities");
    assert_eq!(span.kind, i32::from(SpanKind::Server));
    assert_eq!(hex(&span.parent_span_id), "b7ad6b7169203331");
    assert_eq!(
        attribute(&span.attributes, "http.route").as_deref(),
        Some("/v1/capabilities")
    );
    // The status is a number, as the conventions define it and the collector's sampling compares
    // it, and the span carries only the request's own fields, all from the list of names.
    let status = span
        .attributes
        .iter()
        .find(|pair| pair.key == "http.response.status_code")
        .and_then(|pair| pair.value.as_ref()?.value.clone());
    assert_eq!(status, Some(any_value::Value::IntValue(200)));
    let mut keys: Vec<&str> = span
        .attributes
        .iter()
        .map(|pair| pair.key.as_str())
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "http.request.method",
            "http.response.status_code",
            "http.route"
        ]
    );
    assert!(span.events.is_empty(), "{:?}", span.events);
    let (resource, record) = received
        .records()
        .into_iter()
        .find(|(_, record)| hex(&record.trace_id) == trace)
        .unwrap();
    assert_names_the_process(resource, "production");
    assert_eq!(hex(&record.span_id), hex(&span.span_id));
    assert_eq!(record.severity_text, "INFO");
    assert_eq!(
        attribute(&record.attributes, "http.response.status_code").as_deref(),
        Some("200")
    );
}

#[tokio::test]
async fn platform_040_a_method_http_doesn_t_define_is_other_in_the_span() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let endpoint = collector.endpoint();
    let server = database
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint),
        ])
        .await;
    let trace = "5bf1a8b1a1e04e7a8c0f1d2e3a4b5c6d";

    let response = server
        .request(
            "PROPFIND",
            "/v1/capabilities",
            &[("traceparent", &format!("00-{trace}-b7ad6b7169203331-01"))],
        )
        .await;
    let received = collector
        .wait_for(|received| {
            received
                .spans()
                .iter()
                .any(|(_, span)| hex(&span.trace_id) == trace)
        })
        .await;

    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
    let (_, span) = received
        .spans()
        .into_iter()
        .find(|(_, span)| hex(&span.trace_id) == trace)
        .unwrap();
    assert_eq!(span.name, "HTTP /v1/capabilities");
    assert_eq!(
        attribute(&span.attributes, "http.request.method").as_deref(),
        Some("_OTHER")
    );
}

#[tokio::test]
async fn platform_042_a_subcommand_writes_out_its_telemetry_before_it_exits() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;

    let output = database
        .run_api(
            &["migrate"],
            &[
                ("ENVIRONMENT", "test"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint()),
            ],
        )
        .await;

    assert!(output.status.success(), "{output:?}");
    let received = collector.received();
    let records = received.records();
    let (resource, _) = records
        .iter()
        .find(|(_, record)| {
            record.body.as_ref().and_then(text).as_deref() == Some("migrations applied")
        })
        .expect("the record of the migrations");
    assert_names_the_process(resource, "test");
    let (resource, _) = received.metrics()[0];
    assert_names_the_process(resource, "test");
    let exported = received.count("otel.sdk.exporter.log.exported", None);
    assert!(exported >= 1, "{exported}");
}

#[tokio::test]
async fn platform_039_each_process_exports_as_an_instance_of_its_own() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let env = [
        ("ENVIRONMENT", "test".to_owned()),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", collector.endpoint()),
    ];

    for _ in 0..2 {
        let output = database.run_api(&["migrate"], &env).await;
        assert!(output.status.success(), "{output:?}");
    }

    let received = collector.received();
    let mut instances: Vec<String> = received
        .records()
        .into_iter()
        .map(|(resource, _)| assert_names_the_process(resource, "test"))
        .collect();
    instances.sort();
    instances.dedup();
    assert_eq!(instances.len(), 2, "{instances:?}");
    let mut measured: Vec<String> = received
        .metrics()
        .into_iter()
        .map(|(resource, _)| assert_names_the_process(resource, "test"))
        .collect();
    measured.sort();
    measured.dedup();
    assert_eq!(measured, instances);
}

#[tokio::test]
async fn platform_042_a_subcommand_writes_out_its_spans_before_it_exits() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;

    // bootstrap computes each role's password verifier in a span of its own (OBS-06).
    let output = database
        .with_roles_locked(database.run_api(
            &["bootstrap"],
            &[
                ("ENVIRONMENT", "test"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint()),
            ],
        ))
        .await;

    assert!(output.status.success(), "{output:?}");
    let received = collector.received();
    let verifiers = received
        .spans()
        .into_iter()
        .filter(|(_, span)| span.name == "scram verifier")
        .count();
    assert_eq!(verifiers, 2, "one for each role");
}

#[tokio::test]
async fn platform_042_the_last_metrics_count_the_exports_that_failed() {
    let collector = Collector::refusing_logs().await.unwrap();
    let database = TestDatabase::migrated().await;

    let output = database
        .run_api(
            &["migrate"],
            &[
                ("ENVIRONMENT", "test"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint()),
            ],
        )
        .await;

    assert!(output.status.success(), "{output:?}");
    let failed = collector
        .received()
        .count("otel.sdk.exporter.log.exported", Some("export_failed"));
    assert!(failed >= 1, "{failed}");
}

#[tokio::test]
async fn obs_10_an_exporter_that_can_t_be_built_fails_startup() {
    let database = TestDatabase::migrated().await;
    let missing = std::env::temp_dir().join("no-such-certificates.pem");

    // An https endpoint needs the certificates SSL_CERT_FILE names, which don't exist.
    let output = database
        .run_api(
            &["migrate"],
            &[
                ("ENVIRONMENT", "test"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://127.0.0.1:4318"),
                ("SSL_CERT_FILE", missing.to_str().unwrap()),
            ],
        )
        .await;

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<serde_json::Value> = stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert_eq!(lines[0]["message"], "startup failed");
    assert_eq!(lines[0]["startup.area"], "telemetry");
}

#[tokio::test]
async fn obs_10_an_export_that_fails_is_counted_and_the_subcommand_goes_on() {
    let collector = Collector::refusing_logs().await.unwrap();
    let database = TestDatabase::migrated().await;

    let output = database
        .run_api(
            &["migrate"],
            &[
                ("ENVIRONMENT", "test"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint()),
            ],
        )
        .await;

    assert!(output.status.success(), "{output:?}");
    let received = collector.received();
    assert!(received.logs.is_empty());
    let failed = received.count("otel.sdk.exporter.log.exported", Some("export_failed"));
    assert!(failed >= 1, "{failed}");
}
