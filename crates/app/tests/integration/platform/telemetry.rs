//! The export of telemetry over OTLP (PLATFORM-066, PLATFORM-068, OBS-10), the counts of what its
//! queues drop (PLATFORM-106), the metrics the pipeline and the readiness check record
//! (PLATFORM-067, PLATFORM-069), and what the health probes leave out of the telemetry (OBS-15,
//! PLATFORM-044).

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use http::StatusCode;
use opentelemetry_proto::tonic::{
    common::v1::any_value,
    metrics::v1::metric,
    resource::v1::Resource,
    trace::v1::{Span, span::SpanKind},
};
use platform::telemetry::testing::{
    self, Point,
    collector::{Collector, Received, attribute, text},
};

use serde_json::json;

use crate::support::{
    Api, CHECK_INTERVAL, Options, Server, TestDatabase, counts, fixtures, hex, jobs, mailbox,
    service_name, users,
};

/// The requests whose durations `collector` received, from a `serve` that has stopped, each as
/// its labels and how many it recorded, as [`counts::recordings`] gives them (TEST-07).
fn requests_counted(collector: &Collector) -> Vec<String> {
    counts::recordings(&collector.received(), "http.server.request.duration")
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

/// Whether `span` is the server span of the request in the trace `trace`, which also holds a
/// span for each of the request's database transactions (PLATFORM-104).
fn is_the_request_s(span: &Span, trace: &str) -> bool {
    hex(&span.trace_id) == trace && span.kind == i32::from(SpanKind::Server)
}

/// The value of `span`'s attribute `key`, of the type the collector received it as.
fn value(span: &Span, key: &str) -> Option<any_value::Value> {
    span.attributes
        .iter()
        .find(|pair| pair.key == key)
        .and_then(|pair| pair.value.as_ref()?.value.clone())
}

/// What `collector` received from a `serve` that ran a queued job of `platform.send_email`, once
/// it holds the span of the run and that of its send.
async fn an_email_s_spans(collector: &Collector) -> Received {
    let database = TestDatabase::migrated().await;
    let payload = json!({
        "message_id": "<numbers@example.com>",
        "email": {
            "to": mailbox::address("numbers"),
            "subject": "Numbers",
            "text": "A run's numbers.",
            "html": "<p>A run's numbers.</p>",
        },
    });
    jobs::enqueued(
        &database,
        "platform.send_email",
        &payload,
        "00-8a3c60f7d188f8fa79d48a391a778fa6-00f067aa0ba902b7-01",
    )
    .await;
    let _server = database
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint()),
        ])
        .await;
    collector
        .wait_for(|received| {
            let arrived = |name: &str| received.spans().iter().any(|(_, span)| span.name == name);
            arrived("platform.send_email") && arrived("smtp send")
        })
        .await
}

/// The span named `name` among `received`.
fn named<'a>(received: &'a Received, name: &str) -> &'a Span {
    received
        .spans()
        .into_iter()
        .map(|(_, span)| span)
        .find(|span| span.name == name)
        .unwrap()
}

#[tokio::test]
async fn platform_067_a_request_records_its_method_route_and_status() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database.exporting_to(&collector, &[]).await;

    let response = server.request("GET", "/v1/capabilities", &[]).await;
    server.stop().await;

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert_eq!(
        requests_counted(&collector),
        ["http.request.method=GET http.response.status_code=200 http.route=/v1/capabilities 1"]
    );
}

#[tokio::test]
async fn platform_067_a_5xx_records_its_error_code_as_its_error_type() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    // A registration issues a device token (the auth spec's "Registration"), whose insert `app`
    // may not make here, so the registration answers 500.
    database
        .owner_executes("REVOKE INSERT ON device_tokens FROM app")
        .await;
    let server = database.exporting_to(&collector, &[]).await;
    let registration = users::registration("ada@example.com").to_string();

    let (status, _) = server
        .send(
            "POST",
            "/v1/users",
            &[("content-type", "application/json")],
            registration.as_bytes(),
        )
        .await;
    server.stop().await;

    assert_eq!(status, 500);
    assert_eq!(
        requests_counted(&collector),
        [
            "error.type=request.internal_error http.request.method=POST \
             http.response.status_code=500 http.route=/v1/users 1"
        ]
    );
}

#[tokio::test]
async fn platform_067_a_503_of_a_feature_turned_off_records_its_code_as_its_error_type() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database.exporting_to(&collector, &[("EMAIL", "off")]).await;
    let reset = json!({ "email": "ada@example.com" }).to_string();

    let (status, _) = server
        .send(
            "POST",
            "/v1/password-resets",
            &[("content-type", "application/json")],
            reset.as_bytes(),
        )
        .await;
    server.stop().await;

    assert_eq!(status, 503);
    assert_eq!(
        requests_counted(&collector),
        [
            "error.type=request.feature_disabled http.request.method=POST \
             http.response.status_code=503 http.route=/v1/password-resets 1"
        ]
    );
}

#[tokio::test]
async fn platform_067_a_method_http_doesn_t_define_is_other() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database.exporting_to(&collector, &[]).await;
    let trace = "9c1f04e2b6a84d0f8e3a7b5c2d1e0f6a";

    let response = server
        .request(
            "PROPFIND",
            "/v1/capabilities",
            &[("traceparent", &format!("00-{trace}-b7ad6b7169203331-01"))],
        )
        .await;
    let written = server.stop().await;

    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
    assert_eq!(
        requests_counted(&collector),
        ["http.request.method=_OTHER http.response.status_code=405 http.route=/v1/capabilities 1"]
    );
    let lines: Vec<_> = written
        .into_iter()
        .filter(|line| line["trace_id"] == trace)
        .collect();
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(lines[0]["http.request.method"], "_OTHER");
}

#[tokio::test]
async fn platform_067_a_request_no_route_matched_records_no_route() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database.exporting_to(&collector, &[]).await;

    let response = server.request("GET", "/v1/no-such-route", &[]).await;
    server.stop().await;

    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    // Exactly these labels: no `http.route`, and no `error.type` on a 4xx.
    assert_eq!(
        requests_counted(&collector),
        ["http.request.method=GET http.response.status_code=404 1"]
    );
}

#[tokio::test]
async fn platform_067_a_request_records_how_long_it_took() {
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
    // The two seconds pass on the clock that the pipeline measures with, which a test moves only
    // for an API in its own process: a `serve` measures on the system's clock (TEST-06). Other
    // tests hold the gate too, on clocks of their own, so the seconds grow by at least these
    // (TEST-07).
    let took = seconds(&durations(), &labels) - before;
    assert!(took >= 2.0, "{took}");
}

#[tokio::test]
async fn platform_067_patch_is_a_method_http_defines() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database.exporting_to(&collector, &[]).await;

    let response = server.request("PATCH", "/v1/capabilities", &[]).await;
    server.stop().await;

    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
    assert_eq!(
        requests_counted(&collector),
        ["http.request.method=PATCH http.response.status_code=405 http.route=/v1/capabilities 1"]
    );
}

#[tokio::test]
async fn platform_067_query_is_a_method_http_defines() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database.exporting_to(&collector, &[]).await;

    let response = server.request("QUERY", "/v1/capabilities", &[]).await;
    server.stop().await;

    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
    assert_eq!(
        requests_counted(&collector),
        ["http.request.method=QUERY http.response.status_code=405 http.route=/v1/capabilities 1"]
    );
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
async fn obs_15_a_health_probe_carries_no_trace_id() {
    let api = Api::new().await;

    let live = api.get("/health/live").send().await;
    let ready = api.get("/health/ready").send().await;

    for probe in [&live, &ready] {
        assert_eq!(probe.status, StatusCode::OK);
        assert!(
            probe.headers.get("server-timing").is_none(),
            "{:?}",
            probe.headers
        );
    }
}

#[tokio::test]
async fn platform_044_a_health_probe_writes_no_line() {
    let api = Api::new().await;

    for path in ["/health/live", "/health/ready"] {
        assert_eq!(api.get(path).send().await.status, StatusCode::OK);
    }

    // A line of a probe's would hold its route, as every completion line does.
    let lines: Vec<serde_json::Value> = testing::all_logged()
        .into_iter()
        .filter(|line| {
            line["http.route"]
                .as_str()
                .is_some_and(|route| route.starts_with("/health"))
        })
        .collect();
    assert!(lines.is_empty(), "{lines:#?}");
}

#[tokio::test]
async fn obs_15_serve_exports_no_span_of_a_health_probe() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint()),
        ])
        .await;
    let (probed, after) = (
        "6e0c63257de34c926f9efcd03927272e",
        "a3ce929d0e0e47364bf92f3577b34da6",
    );
    let parent = |trace: &str| format!("00-{trace}-b7ad6b7169203331-01");

    let live = server
        .request("GET", "/health/live", &[("traceparent", &parent(probed))])
        .await;
    let ready = server
        .request("GET", "/health/ready", &[("traceparent", &parent(probed))])
        .await;
    // A probe's span would end before the next request's spans begin, so its export would come
    // no later than theirs.
    server
        .request(
            "GET",
            "/v1/capabilities",
            &[("traceparent", &parent(after))],
        )
        .await;
    let received = collector
        .wait_for(|received| {
            received
                .spans()
                .iter()
                .any(|(_, span)| is_the_request_s(span, after))
        })
        .await;

    assert!(live.starts_with("HTTP/1.1 200"), "{live}");
    assert!(ready.starts_with("HTTP/1.1 200"), "{ready}");
    let probes: Vec<&str> = received
        .spans()
        .into_iter()
        .map(|(_, span)| span)
        .filter(|span| hex(&span.trace_id) == probed || span.name.contains("/health"))
        .map(|span| span.name.as_str())
        .collect();
    assert_eq!(probes, <[&str; 0]>::default());
}

#[tokio::test]
async fn platform_070_the_resident_memory_is_recorded_at_once_and_every_15_seconds_after() {
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
async fn platform_069_the_database_gauge_follows_each_check() {
    let api = Api::new().await;
    let database = api.database();
    let clock = api.clock();
    let reachable = || {
        testing::recorded("platform.database_reachable")
            .into_iter()
            .find(|point| point.labels.get("db.namespace") == Some(&database.name().to_owned()))
            .map(|point| point.value)
    };
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
async fn platform_066_serve_exports_its_spans_and_log_records_with_their_trace_context() {
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
            .any(|(_, span)| is_the_request_s(span, trace))
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
        .find(|(_, span)| is_the_request_s(span, trace))
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
async fn platform_067_a_method_http_doesn_t_define_is_other_in_the_span() {
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
                .any(|(_, span)| is_the_request_s(span, trace))
        })
        .await;

    assert!(response.starts_with("HTTP/1.1 405"), "{response}");
    let (_, span) = received
        .spans()
        .into_iter()
        .find(|(_, span)| is_the_request_s(span, trace))
        .unwrap();
    assert_eq!(span.name, "HTTP /v1/capabilities");
    assert_eq!(
        attribute(&span.attributes, "http.request.method").as_deref(),
        Some("_OTHER")
    );
}

#[tokio::test]
async fn platform_068_a_subcommand_writes_out_its_telemetry_before_it_exits() {
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
    // The last metrics count each record that the collector took.
    let exported = received.count("otel.sdk.exporter.log.exported", None);
    assert_eq!(exported, i64::try_from(records.len()).unwrap());
}

#[tokio::test]
async fn platform_066_each_process_exports_as_an_instance_of_its_own() {
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
async fn platform_066_a_job_run_s_span_exports_its_attempt_as_a_number() {
    let collector = Collector::start().await.unwrap();

    let received = an_email_s_spans(&collector).await;

    let run = named(&received, "platform.send_email");
    assert_eq!(
        value(run, "job.attempt"),
        Some(any_value::Value::IntValue(1))
    );
}

#[tokio::test]
async fn platform_066_the_smtp_span_exports_the_server_s_port_as_a_number() {
    let collector = Collector::start().await.unwrap();

    let received = an_email_s_spans(&collector).await;

    // The mail server's port in `.env`, which the tests' settings have read by now.
    let port: i64 = std::env::var("SMTP_PORT").unwrap().parse().unwrap();
    let send = named(&received, "smtp send");
    assert_eq!(
        value(send, "server.port"),
        Some(any_value::Value::IntValue(port))
    );
}

#[tokio::test]
async fn platform_068_a_subcommand_writes_out_its_spans_before_it_exits() {
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
async fn platform_068_the_last_metrics_count_the_exports_that_failed() {
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
    let failed = received.count("otel.sdk.exporter.log.exported", Some("export_failed"));
    let handed = received.count("otel.sdk.processor.log.processed", None);
    // The last metrics count as failed each record that the queue handed its exporter, as many
    // as the subcommand wrote.
    assert!(handed >= 1, "{handed}");
    assert_eq!(failed, handed);
}

#[tokio::test]
async fn sec_10_an_exporter_follows_no_redirect() {
    let collector = Collector::redirecting_logs().await.unwrap();
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
    // The path that the redirect names takes the records of an exporter that follows it.
    let received = collector.received();
    assert!(received.logs.is_empty());
    let failed = received.count("otel.sdk.exporter.log.exported", Some("export_failed"));
    let handed = received.count("otel.sdk.processor.log.processed", None);
    // The last metrics count as failed each record that the queue handed its exporter, as many
    // as the subcommand wrote.
    assert!(handed >= 1, "{handed}");
    assert_eq!(failed, handed);
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
    let handed = received.count("otel.sdk.processor.log.processed", None);
    // The last metrics count as failed each record that the queue handed its exporter, as many
    // as the subcommand wrote.
    assert!(handed >= 1, "{handed}");
    assert_eq!(failed, handed);
}

/// Each metric of the scope in which the SDK counts itself, among every export of `received`, by
/// its name, with the names of the attributes its data points carry.
fn sdk_metrics(received: &Received) -> BTreeMap<String, Vec<String>> {
    let mut metrics: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let scopes = received
        .metrics
        .iter()
        .flat_map(|export| &export.resource_metrics)
        .flat_map(|resource| &resource.scope_metrics)
        .filter(|scope| {
            scope
                .scope
                .as_ref()
                .is_some_and(|scope| scope.name == "otel.sdk")
        });
    for metric in scopes.flat_map(|scope| &scope.metrics) {
        let keys = metrics.entry(metric.name.clone()).or_default();
        if let Some(metric::Data::Sum(sum)) = &metric.data {
            keys.extend(
                sum.data_points
                    .iter()
                    .flat_map(|point| &point.attributes)
                    .map(|pair| pair.key.clone()),
            );
        }
        keys.sort();
        keys.dedup();
    }
    metrics
}

/// Sends `server` `requests` requests for the capabilities, 16 at a time, so a queue fills
/// before a held export's timeout lets its processor take another batch. Each ends at least two
/// spans, its own and its rate limit's count, and writes a log record.
async fn flood(server: &Server, requests: u16) {
    for _ in 0..requests / 16 {
        futures_util::future::join_all(
            (0..16).map(|_| server.request("GET", "/v1/capabilities", &[])),
        )
        .await;
    }
}

#[tokio::test]
async fn platform_106_a_full_queue_of_spans_counts_what_it_drops_and_the_sdk_exports_no_other_count()
 {
    const REQUESTS: u16 = 2_000;
    let collector = Collector::holding_traces().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database.exporting_to(&collector, &[]).await;

    flood(&server, REQUESTS).await;
    // The log records aren't held: the metrics that count their export were collected after
    // the requests' spans ended.
    let received = collector
        .wait_for(|received| {
            received.count("otel.sdk.exporter.log.exported", None) >= i64::from(REQUESTS)
        })
        .await;

    let dropped = received.count("otel.sdk.processor.span.processed", Some("queue_full"));
    let handed = received.count("otel.sdk.processor.span.processed", None);
    // How many spans come while the queue is full, the requests' pace against the held export's
    // timeout decides, so each count is only asked to be some.
    assert!(dropped > 0, "{dropped}");
    assert!(handed > 0, "{handed}");
    assert_eq!(
        sdk_metrics(&received),
        BTreeMap::from([
            (
                "otel.sdk.processor.log.processed".to_owned(),
                vec!["error.type".to_owned()]
            ),
            (
                "otel.sdk.processor.span.processed".to_owned(),
                vec!["error.type".to_owned()]
            ),
        ])
    );
}

#[tokio::test]
async fn platform_106_a_full_queue_of_log_records_counts_what_it_drops() {
    const REQUESTS: u16 = 4_000;
    let collector = Collector::holding_logs().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database.exporting_to(&collector, &[]).await;

    flood(&server, REQUESTS).await;
    // The spans aren't held: the metrics that count their export were collected after the
    // requests wrote their log records.
    let received = collector
        .wait_for(|received| {
            received.count("otel.sdk.exporter.span.exported", None) >= 2 * i64::from(REQUESTS)
        })
        .await;

    let dropped = received.count("otel.sdk.processor.log.processed", Some("queue_full"));
    let handed = received.count("otel.sdk.processor.log.processed", None);
    // How many records come while the queue is full, the requests' pace against the held
    // export's timeout decides, so each count is only asked to be some.
    assert!(dropped > 0, "{dropped}");
    assert!(handed > 0, "{handed}");
}
