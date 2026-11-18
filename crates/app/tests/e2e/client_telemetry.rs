//! Clients' telemetry through the image and the observability stack that Compose runs
//! (CLIENT_TELEMETRY-011, CLIENT_TELEMETRY-012): what a consenting user's client sends the API
//! reaches Tempo and Loki through the collector's pipelines of clients' telemetry, which keep
//! the listed services' alone, and of each resource only what names the client.

use std::net::SocketAddr;

use serde_json::{Value, json};

use crate::{
    docker::{Container, lossy},
    image::PORT,
    support::{self, TestDatabase, http_exchange, users},
    traces::{fetch_once_it_holds, serve_with_the_stack, why_nothing_arrived},
};

/// The service that the collector's configuration of clients' telemetry lists.
const CLIENT: &str = "app-desktop";

/// Sends `method` to `path` on `address`, with `headers` and the JSON `body`, and returns the
/// response's status and its body.
async fn send(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &Value,
) -> (u16, String) {
    let body = body.to_string();
    let length = body.len().to_string();
    let mut headers = headers.to_vec();
    headers.extend([
        ("Content-Type", "application/json"),
        ("Content-Length", &length),
    ]);
    let response = http_exchange(address, method, path, &headers, &body)
        .await
        .expect("an answer");
    let status = response
        .split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .expect("a status");
    let body = response.split("\r\n\r\n").nth(1).unwrap_or_default();
    (status, body.to_owned())
}

/// A replica of the image that forwards its clients' telemetry to the stack's collector, and the
/// `Authorization` of a user of its own who consents to their clients' telemetry.
async fn forwarding(database: &TestDatabase) -> (Container, String) {
    let server = serve_with_the_stack(
        database,
        &["CLIENT_TELEMETRY_ENDPOINT=http://collector:4319"],
    )
    .await;
    let address = server.address(PORT);
    let registration = users::registration("ada@example.com");
    let (registered, session) = send(address, "POST", "/v1/users", &[], &registration).await;
    assert_eq!(registered, 201, "{session}");
    let session: Value = serde_json::from_str(&session).expect("a session");
    let bearer = format!(
        "Bearer {}",
        session["access_token"].as_str().expect("a token")
    );
    let (consented, body) = send(
        address,
        "PUT",
        "/v1/users/me/telemetry-consent",
        &[("Authorization", &bearer)],
        &json!({ "consented": true }),
    )
    .await;
    assert_eq!(consented, 200, "{body}");
    (server, bearer)
}

/// A trace ID and a span ID that no other test's telemetry has.
fn ids() -> (String, String) {
    let trace = uuid::Uuid::new_v4().simple().to_string();
    let span = uuid::Uuid::new_v4().simple().to_string()[..16].to_owned();
    (trace, span)
}

/// The moment, by the tests' database, in nanoseconds since the epoch, as the protocol's JSON
/// holds a time.
async fn now(database: &TestDatabase) -> String {
    database
        .owner_reads("SELECT (extract(epoch FROM now()) * 1000000000)::bigint::text")
        .await
        .remove(0)
}

/// The resource that a client's SDK sends: `service`, if any, the client's version and its
/// installation, and the name of its user's machine.
fn resource(service: Option<&str>) -> Value {
    let attribute =
        |key: &str, value: &str| json!({ "key": key, "value": { "stringValue": value } });
    let mut attributes = vec![
        attribute("service.version", "1.2.3"),
        attribute("service.instance.id", "installation-42"),
        attribute("host.name", "adas-laptop"),
    ];
    attributes.extend(service.map(|service| attribute("service.name", service)));
    json!({ "attributes": attributes })
}

/// A batch of one span of `trace`, named `name`, from `service`.
fn spans(service: Option<&str>, trace: &str, span: &str, name: &str, at: &str) -> Value {
    json!({ "resourceSpans": [{
        "resource": resource(service),
        "scopeSpans": [{ "scope": { "name": "client" }, "spans": [{
            "traceId": trace, "spanId": span, "name": name, "kind": 1,
            "startTimeUnixNano": at, "endTimeUnixNano": at,
        }] }],
    }] })
}

/// A batch of one log record of `trace`, whose line is `line`, from `service`.
fn records(service: Option<&str>, trace: &str, span: &str, line: &str, at: &str) -> Value {
    json!({ "resourceLogs": [{
        "resource": resource(service),
        "scopeLogs": [{ "scope": { "name": "client" }, "logRecords": [{
            "timeUnixNano": at, "severityNumber": 9, "severityText": "INFO",
            "body": { "stringValue": line }, "traceId": trace, "spanId": span,
        }] }],
    }] })
}

/// The address of Loki's query for the lines of `trace` from services that match `services`.
fn lines_of(trace: &str, services: &str) -> String {
    format!(
        "http://loki:3100/loki/api/v1/query_range?query=%7Bservice_name%3D~%22{services}%22%7D%20%7C%20trace_id%3D%22{trace}%22"
    )
}

#[tokio::test]
async fn client_telemetry_011_a_client_s_spans_and_log_records_reach_tempo_and_loki() {
    let database = TestDatabase::empty().await;
    let (server, bearer) = forwarding(&database).await;
    let (address, authorization) = (server.address(PORT), [("Authorization", bearer.as_str())]);
    let (trace, span) = ids();
    let at = now(&database).await;

    let sent_spans = send(
        address,
        "POST",
        "/v1/users/me/telemetry/traces",
        &authorization,
        &spans(Some(CLIENT), &trace, &span, "settings opened", &at),
    )
    .await;
    let sent_records = send(
        address,
        "POST",
        "/v1/users/me/telemetry/logs",
        &authorization,
        &records(Some(CLIENT), &trace, &span, "the settings opened", &at),
    )
    .await;
    // A request of the client's own trace, which ends in an error, so the collector keeps the
    // API's spans of it (OBS-08).
    let parent = format!("00-{trace}-{span}-01");
    let failed = send(
        address,
        "GET",
        "/v1/no-such-resource",
        &[("traceparent", &parent)],
        &json!({}),
    )
    .await;
    let stored = fetch_once_it_holds(
        &format!("http://tempo:3200/api/traces/{trace}"),
        support::service_name(),
    );
    let lines = fetch_once_it_holds(&lines_of(&trace, CLIENT), "the settings opened");

    assert_eq!(sent_spans, (200, "{}".to_owned()));
    assert_eq!(sent_records, (200, "{}".to_owned()));
    assert_eq!(failed.0, 404, "{}", failed.1);
    assert!(
        stored.status.success() && lines.status.success(),
        "{}\n{}\n{}",
        lossy(&stored),
        lossy(&lines),
        why_nothing_arrived(&server)
    );
    // The trace holds the client's span beside the API's, under what names the client alone.
    let (stored, lines) = (lossy(&stored), lossy(&lines));
    for kept in [CLIENT, "settings opened", "1.2.3", "installation-42"] {
        assert!(stored.contains(kept), "no {kept} in {stored}");
    }
    assert!(!stored.contains("adas-laptop"), "{stored}");
    // The log record keeps the client's name and version, and no installation.
    assert!(lines.contains("1.2.3"), "{lines}");
    for dropped in ["installation-42", "adas-laptop"] {
        assert!(!lines.contains(dropped), "{dropped} in {lines}");
    }
}

#[tokio::test]
async fn client_telemetry_012_the_collector_stores_nothing_of_a_service_it_doesn_t_list() {
    let database = TestDatabase::empty().await;
    let (server, bearer) = forwarding(&database).await;
    let (address, authorization) = (server.address(PORT), [("Authorization", bearer.as_str())]);
    let (trace, span) = ids();
    let at = now(&database).await;
    let api = support::service_name();

    // The telemetry of a service that the collector doesn't list, the API's own name among them,
    // and of one without a name, then that of a listed one, which the pipelines pass last.
    for (service, line) in [
        (Some(api), "a line that passes for the API's"),
        (
            Some("another-client"),
            "a line of a client that isn't listed",
        ),
        (None, "a line of no service"),
        (Some(CLIENT), "a line of a listed client"),
    ] {
        let (traces, logs) = (
            spans(service, &trace, &span, line, &at),
            records(service, &trace, &span, line, &at),
        );
        let sent = send(
            address,
            "POST",
            "/v1/users/me/telemetry/traces",
            &authorization,
            &traces,
        )
        .await;
        assert_eq!(sent, (200, "{}".to_owned()), "{service:?}");
        let sent = send(
            address,
            "POST",
            "/v1/users/me/telemetry/logs",
            &authorization,
            &logs,
        )
        .await;
        assert_eq!(sent, (200, "{}".to_owned()), "{service:?}");
    }
    let stored = fetch_once_it_holds(
        &format!("http://tempo:3200/api/traces/{trace}"),
        "a line of a listed client",
    );
    let lines = fetch_once_it_holds(&lines_of(&trace, ".%2B"), "a line of a listed client");

    assert!(
        stored.status.success() && lines.status.success(),
        "{}\n{}\n{}",
        lossy(&stored),
        lossy(&lines),
        why_nothing_arrived(&server)
    );
    for kept in [lossy(&stored), lossy(&lines)] {
        for dropped in [
            "a line that passes for the API's",
            "a line of a client that isn't listed",
            "a line of no service",
            "another-client",
        ] {
            assert!(!kept.contains(dropped), "{dropped} in {kept}");
        }
    }
}

#[tokio::test]
async fn client_telemetry_009_the_stack_s_collector_refuses_a_body_that_isn_t_the_protocol_s_request()
 {
    let database = TestDatabase::empty().await;
    let (server, bearer) = forwarding(&database).await;
    let (address, authorization) = (server.address(PORT), [("Authorization", bearer.as_str())]);

    let (refused, problem) = send(
        address,
        "POST",
        "/v1/users/me/telemetry/traces",
        &authorization,
        &json!({ "resourceSpans": 5 }),
    )
    .await;

    assert_eq!(refused, 400, "{problem}\n{}", server.logs());
    let problem: Value = serde_json::from_str(&problem).expect("a problem");
    assert_eq!(problem["code"], "client_telemetry.batch_refused");
}
