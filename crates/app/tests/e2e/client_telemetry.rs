//! Clients' telemetry through the image and the observability stack that Compose runs
//! (CLIENT_TELEMETRY-011, CLIENT_TELEMETRY-012, CLIENT_TELEMETRY-014, CLIENT_TELEMETRY-015): what
//! a consenting user's client sends the API reaches Tempo, Loki and Prometheus through the
//! collector's pipelines of clients' telemetry, which keep the listed services' alone, of each
//! resource only what names the client, and of the metrics what the configuration lists, added up
//! over every installation. And through a deployment whose telemetry goes to a managed backend
//! (CLIENT_TELEMETRY-016).

use std::{net::SocketAddr, process::Output, time::Duration};

use serde_json::{Value, json};

use crate::{
    deployment::{Deployment, get_within, post, post_as, put_as},
    docker::{Container, lossy},
    image::PORT,
    support::{self, TestDatabase, http_exchange, users},
    traces::{fetch_once_it_holds, in_stack, serve_with_the_stack, why_nothing_arrived},
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

/// What a shell of the stack printed: its standard output, without what Compose reports of the
/// container that it ran in, whose name holds the project's.
fn printed(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
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

/// An attribute whose value is a string, as the protocol's JSON holds one.
fn attribute(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}

/// The resource that a client's SDK sends: `service`, if any, the client's version and its
/// installation, and the name of its user's machine.
fn resource(service: Option<&str>) -> Value {
    let mut attributes = vec![
        attribute("service.version", "1.2.3"),
        attribute("service.instance.id", "installation-42"),
        attribute("host.name", "adas-laptop"),
    ];
    attributes.extend(service.map(|service| attribute("service.name", service)));
    json!({ "attributes": attributes })
}

/// A resource that names its service twice, as `first` and then as `second`, which the protocol
/// forbids and the collector's receiver takes.
fn named_twice(first: &str, second: &str) -> Value {
    json!({ "attributes": [
        attribute("service.name", first),
        attribute("service.name", second),
        attribute("service.version", "1.2.3"),
    ] })
}

/// A batch of one span of `trace`, named `name`, from `service`.
fn spans(service: Option<&str>, trace: &str, span: &str, name: &str, at: &str) -> Value {
    spans_of(&resource(service), trace, span, name, at)
}

/// A batch of one span of `trace`, named `name`, of `resource`.
fn spans_of(resource: &Value, trace: &str, span: &str, name: &str, at: &str) -> Value {
    json!({ "resourceSpans": [{
        "resource": resource,
        "scopeSpans": [{ "scope": { "name": "client" }, "spans": [{
            "traceId": trace, "spanId": span, "name": name, "kind": 1,
            "startTimeUnixNano": at, "endTimeUnixNano": at,
        }] }],
    }] })
}

/// A batch of one log record of `trace`, whose line is `line`, from `service`.
fn records(service: Option<&str>, trace: &str, span: &str, line: &str, at: &str) -> Value {
    records_of(&resource(service), trace, span, line, at)
}

/// A batch of one log record of `trace`, whose line is `line`, of `resource`.
fn records_of(resource: &Value, trace: &str, span: &str, line: &str, at: &str) -> Value {
    json!({ "resourceLogs": [{
        "resource": resource,
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
    // The client's span and the API's reach Tempo through pipelines of their own, so the test
    // waits for each: the trace that holds the API's span then still holds the client's.
    let stored_trace = format!("http://tempo:3200/api/traces/{trace}");
    let clients = fetch_once_it_holds(&stored_trace, "installation-42");
    let stored = fetch_once_it_holds(&stored_trace, support::service_name());
    let lines = fetch_once_it_holds(&lines_of(&trace, CLIENT), "the settings opened");

    assert_eq!(sent_spans, (200, "{}".to_owned()));
    assert_eq!(sent_records, (200, "{}".to_owned()));
    assert_eq!(failed.0, 404, "{}", failed.1);
    assert!(
        clients.status.success() && stored.status.success() && lines.status.success(),
        "{}\n{}\n{}\n{}",
        lossy(&clients),
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
async fn client_telemetry_012_a_resource_that_names_its_service_twice_is_stored_under_the_first_name_alone()
 {
    let database = TestDatabase::empty().await;
    let (server, bearer) = forwarding(&database).await;
    let (address, authorization) = (server.address(PORT), [("Authorization", bearer.as_str())]);
    let (trace, span) = ids();
    let at = now(&database).await;
    let api = support::service_name();

    // A resource whose first name isn't listed, and whose second is, then one whose first name
    // is listed, and whose second is the API's own, which the pipelines pass last.
    for (resource, line) in [
        (named_twice(api, CLIENT), "a line under a first name"),
        (named_twice(CLIENT, api), "a line under a second name"),
    ] {
        let sent = send(
            address,
            "POST",
            "/v1/users/me/telemetry/traces",
            &authorization,
            &spans_of(&resource, &trace, &span, line, &at),
        )
        .await;
        assert_eq!(sent, (200, "{}".to_owned()), "{line}");
        let sent = send(
            address,
            "POST",
            "/v1/users/me/telemetry/logs",
            &authorization,
            &records_of(&resource, &trace, &span, line, &at),
        )
        .await;
        assert_eq!(sent, (200, "{}".to_owned()), "{line}");
    }
    // The line is the listed client's, and no other service's.
    let stored = fetch_once_it_holds(
        &format!("http://tempo:3200/api/traces/{trace}"),
        "a line under a second name",
    );
    let clients = fetch_once_it_holds(&lines_of(&trace, CLIENT), "a line under a second name");
    let lines = fetch_once_it_holds(&lines_of(&trace, ".%2B"), "a line under a second name");

    assert!(
        stored.status.success() && clients.status.success() && lines.status.success(),
        "{}\n{}\n{}\n{}",
        lossy(&stored),
        lossy(&clients),
        lossy(&lines),
        why_nothing_arrived(&server)
    );
    for kept in [printed(&stored), printed(&lines)] {
        assert!(kept.contains(CLIENT), "{kept}");
        for dropped in ["a line under a first name", api] {
            assert!(!kept.contains(dropped), "{dropped} in {kept}");
        }
    }
}

/// Where the stack's collector serves the series that it adds up from the clients' metrics.
const SERVED: &str = "http://collector:9464/metrics";

/// The buckets' boundaries that the collector's configuration lists for the template's metric.
const BOUNDS: [f64; 14] = [
    0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];

/// The series that counts the template's metric's requests, as the collector serves it: of the
/// points that kept `status`, or else `error`, or neither.
fn series(status: Option<u16>, error: Option<&str>) -> String {
    let mut labels = String::new();
    if let Some(error) = error {
        labels.push_str(&format!("error_type=\"{error}\","));
    }
    if let Some(status) = status {
        labels.push_str(&format!("http_response_status_code=\"{status}\","));
    }
    format!("http_client_request_duration_seconds_count{{{labels}job=\"{CLIENT}\"}}")
}

/// What the collector serves of `series` in `served`, if it serves it.
fn value_of(served: &str, series: &str) -> Option<f64> {
    served.lines().find_map(|line| {
        line.strip_prefix(series)?
            .strip_prefix(' ')?
            .trim()
            .parse()
            .ok()
    })
}

/// Everything that the collector serves now.
fn served() -> String {
    let output = in_stack(&format!("wget -qO- '{SERVED}'"));
    assert!(output.status.success(), "{}", lossy(&output));
    printed(&output)
}

/// Waits until the collector serves `series`, with `value` if one is given, and returns what it
/// then serves, of every series. The wait runs in a shell of the stack, as
/// [`fetch_once_it_holds`]'s does.
fn once_served(server: &Container, series: &str, value: Option<f64>) -> String {
    let (flags, wanted) = match value {
        Some(value) => ("-qxF", format!("{series} {value}")),
        None => ("-qF", format!("{series} ")),
    };
    let output = in_stack(&format!(
        "timeout 60 sh -c 'until wget -qO /tmp/served \"$0\" && grep {flags} \"$1\" /tmp/served; do sleep 1; done' \
         '{SERVED}' '{wanted}' && cat /tmp/served"
    ));
    assert!(
        output.status.success(),
        "no {wanted}\n{}\n{}",
        lossy(&output),
        why_nothing_arrived(server)
    );
    printed(&output)
}

/// A point of a histogram that counts `count` requests in its first bucket, with `attributes`.
fn point(attributes: &Value, count: u64, at: &str) -> Value {
    let mut buckets = vec!["0".to_owned(); BOUNDS.len() + 1];
    buckets[0] = count.to_string();
    json!({
        "attributes": attributes, "startTimeUnixNano": at, "timeUnixNano": at,
        "count": count.to_string(), "sum": 0.001, "bucketCounts": buckets,
        "explicitBounds": BOUNDS,
    })
}

/// The template's metric as the collector's configuration lists it, holding `points`: a
/// histogram in seconds, of what the client measured since its last points.
fn listed(points: &[Value]) -> Value {
    json!({
        "name": "http.client.request.duration", "unit": "s",
        "histogram": { "aggregationTemporality": 1, "dataPoints": points },
    })
}

/// One resource's part of a batch of metrics: `metric`, from `resource`, measured by a library
/// that names itself.
fn measured(resource: &Value, metric: &Value) -> Value {
    json!({
        "resource": resource,
        "scopeMetrics": [{
            "scope": { "name": "a-client-library", "version": "9.9.9" },
            "metrics": [metric],
        }],
    })
}

/// The attribute that holds a response's status, as the conventions do: an integer.
fn status(code: u16) -> Value {
    json!({ "key": "http.response.status_code", "value": { "intValue": code.to_string() } })
}

/// Sends `parts`, each one resource's metrics, as one batch to `address`, and asserts that the
/// API took it.
async fn send_metrics(address: SocketAddr, bearer: &str, parts: &[Value]) {
    let sent = send(
        address,
        "POST",
        "/v1/users/me/telemetry/metrics",
        &[("Authorization", bearer)],
        &json!({ "resourceMetrics": parts }),
    )
    .await;
    assert_eq!(sent, (200, "{}".to_owned()));
}

/// Sends one point of `count` requests with `attributes`, from the listed client, and waits until
/// the collector has added it to `series`: to what it served of the series before, or, while it
/// served none, to whatever it still held of it. Returns what the collector then serves of every
/// series. No other test sends points of `series`.
async fn add(
    server: &Container,
    bearer: &str,
    series: &str,
    attributes: &Value,
    count: u64,
    at: &str,
) -> String {
    let before = value_of(&served(), series);
    let part = measured(
        &resource(Some(CLIENT)),
        &listed(&[point(attributes, count, at)]),
    );
    send_metrics(server.address(PORT), bearer, &[part]).await;
    #[expect(clippy::cast_precision_loss, reason = "a test's few requests")]
    let wanted = before.map(|before| before + count as f64);
    once_served(server, series, wanted)
}

#[tokio::test]
async fn client_telemetry_014_a_client_s_metrics_reach_prometheus_added_up_over_its_installations()
{
    let database = TestDatabase::empty().await;
    let (server, bearer) = forwarding(&database).await;
    let at = now(&database).await;
    let ok = series(Some(200), None);
    // What an SDK's instrumentation of HTTP requests sends with a point, beside its status.
    let attributes = json!([
        status(200),
        attribute("http.request.method", "GET"),
        attribute("server.address", "api.example.com"),
        attribute(
            "url.full",
            "https://api.example.com/v1/users/me?token=secret"
        ),
    ]);

    // Three requests that one installation measured, then five of another's.
    let first = add(&server, &bearer, &ok, &attributes, 3, &at).await;
    let first = value_of(&first, &ok).expect("the series");
    let mut other = resource(Some(CLIENT));
    other["attributes"][1] = attribute("service.instance.id", "installation-7");
    let part = measured(&other, &listed(&[point(&attributes, 5, &at)]));
    send_metrics(server.address(PORT), &bearer, &[part]).await;
    let both = once_served(&server, &ok, Some(first + 5.0));
    // Prometheus scrapes the series every 15 seconds.
    let query = "http://prometheus:9090/api/v1/query?query=http_client_request_duration_seconds_count%7Bhttp_response_status_code%3D%22200%22%7D";
    let scraped = in_stack(&format!(
        "timeout 60 sh -c 'until wget -qO /tmp/scraped \"$0\" && grep -qF \"$1\" /tmp/scraped; do sleep 1; done' \
         '{query}' ',\"{}\"]}}' && cat /tmp/scraped",
        first + 5.0
    ));

    assert!(first >= 3.0, "{first}");
    assert!(
        scraped.status.success(),
        "{}\n{}",
        lossy(&scraped),
        why_nothing_arrived(&server)
    );
    // One series, under the client's name, with the status as its one attribute: nothing of the
    // client's version, its installations, its user's machine, its library, or of what else a
    // point held.
    let scraped: Value = serde_json::from_str(&printed(&scraped)).expect("Prometheus's answer");
    let result = scraped["data"]["result"].as_array().expect("the series");
    assert_eq!(result.len(), 1, "{result:?}");
    let labels = result[0]["metric"].as_object().expect("its labels");
    let mut names: Vec<&str> = labels.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["__name__", "http_response_status_code", "instance", "job"]
    );
    assert_eq!(labels["job"], CLIENT);
    assert_eq!(labels["instance"], "collector:9464");
    for dropped in [
        "1.2.3",
        "installation",
        "adas-laptop",
        "a-client-library",
        "GET",
        "secret",
    ] {
        assert!(!both.contains(dropped), "{dropped} in {both}");
    }
}

/// The statuses whose series the test of what the collector keeps reads: 404, under which it
/// keeps points, 500, which one of those points names second, and nine that the points it drops
/// name.
const READ: [u16; 11] = [404, 500, 501, 502, 503, 504, 505, 511, 417, 421, 428];

/// The attribute that says why a request failed, as the conventions name it.
fn error(kind: &str) -> Value {
    attribute("error.type", kind)
}

/// A histogram named `name`, in `unit`, of one point, whose temporality the protocol numbers
/// `temporality`: 1 for a point that holds what the client measured since its last one.
fn named(name: &str, unit: &str, temporality: u8, point: &Value) -> Value {
    json!({
        "name": name, "unit": unit,
        "histogram": { "aggregationTemporality": temporality, "dataPoints": [point] },
    })
}

/// One point in each series that the test of what the collector keeps reads, so that the
/// collector serves them all before the test's other points come: one for each status of
/// [`READ`], one for each kind of failure that the series tell apart, and one that names neither.
fn one_in_each(at: &str) -> Value {
    let mut points: Vec<Value> = READ
        .iter()
        .map(|code| point(&json!([status(*code)]), 1, at))
        .collect();
    for kind in ["timeout", "connection_error", "a kind that isn't listed"] {
        points.push(point(&json!([error(kind)]), 1, at));
    }
    points.push(point(&json!([]), 1, at));
    measured(&resource(Some(CLIENT)), &listed(&points))
}

/// The points that the collector keeps, each in the series that the test expects it in. Under
/// 404: one that names its status beside attributes that the configuration doesn't list, one that
/// names two statuses, and one of a resource that names two services, `api` second. Without a
/// status: one whose status isn't of the lists, and one whose status isn't an integer. By why the
/// request failed: a listed kind, one that isn't listed, and a point that names two kinds.
fn kept(at: &str, api: &str) -> Vec<Value> {
    let client = resource(Some(CLIENT));
    let one = |attributes: Value, count: u64| point(&attributes, count, at);
    vec![
        measured(
            &client,
            &listed(&[
                one(
                    json!([
                        status(404),
                        error("404"),
                        attribute("url.path", "/v1/users/7")
                    ]),
                    1,
                ),
                one(json!([status(404), status(500)]), 10),
            ]),
        ),
        measured(
            &named_twice(CLIENT, api),
            &listed(&[one(json!([status(404)]), 100)]),
        ),
        measured(
            &client,
            &listed(&[
                one(json!([status(299)]), 1),
                one(json!([attribute("http.response.status_code", "200")]), 10),
            ]),
        ),
        measured(
            &client,
            &listed(&[
                one(json!([error("timeout")]), 1),
                one(json!([error("java.net.UnknownHostException")]), 1),
                one(json!([error("connection_error"), error("timeout")]), 1),
            ]),
        ),
    ]
}

/// The points that the collector drops, each of which names a status of [`READ`] that no kept
/// point names: another metric; the listed one whose points hold what the client measured since
/// it started, in milliseconds, in other buckets, with a sum that is no number, that is below
/// 0 or that is above a billion, and with the flag of a point that holds no value; and the
/// listed one from a client that isn't listed, and from a resource whose first name, `api`,
/// isn't listed.
fn dropped(at: &str, api: &str) -> Vec<Value> {
    let client = resource(Some(CLIENT));
    let one = |code: u16| point(&json!([status(code)]), 1, at);
    let mut other_buckets = one(504);
    other_buckets["explicitBounds"] = json!([0.1, 1.0, 10.0]);
    other_buckets["bucketCounts"] = json!(["1", "0", "0", "0"]);
    let mut no_number = one(505);
    no_number["sum"] = json!("NaN");
    let mut no_value = one(511);
    no_value["flags"] = json!(1);
    let mut below = one(417);
    below["sum"] = json!(-0.5);
    let mut above = one(421);
    above["sum"] = json!(2.0e9);
    let metric = "http.client.request.duration";
    vec![
        measured(&client, &named("another.metric", "s", 1, &one(501))),
        measured(&client, &named(metric, "s", 2, &one(502))),
        measured(&client, &named(metric, "ms", 1, &one(503))),
        measured(
            &client,
            &listed(&[other_buckets, no_number, no_value, below, above]),
        ),
        measured(&resource(Some("another-client")), &listed(&[one(428)])),
        measured(&named_twice(api, CLIENT), &listed(&[one(428)])),
    ]
}

/// By how much `series` grew from what the collector served `before` to what it served `after`.
fn grew(before: &str, after: &str, series: &str) -> f64 {
    let value = |served: &str| value_of(served, series).expect("the collector serves the series");
    value(after) - value(before)
}

#[tokio::test]
async fn client_telemetry_015_the_collector_keeps_of_a_client_s_metrics_what_its_configuration_lists()
 {
    let database = TestDatabase::empty().await;
    let (server, bearer) = forwarding(&database).await;
    let address = server.address(PORT);
    let at = now(&database).await;
    let api = support::service_name();
    let mut read: Vec<String> = READ.iter().map(|code| series(Some(*code), None)).collect();
    for kind in ["timeout", "connection_error", "_OTHER"] {
        read.push(series(None, Some(kind)));
    }
    read.push(series(None, None));
    // A point of a series of its own, that of the status 426, ends each step: the pipeline
    // passes the batches on in the order they came, so once the collector has added that point,
    // it has taken what the test sent before it.
    let (last, its_status) = (series(Some(426), None), json!([status(426)]));

    send_metrics(address, &bearer, &[one_in_each(&at)]).await;
    let before = add(&server, &bearer, &last, &its_status, 1, &at).await;
    let mut parts = kept(&at, api);
    parts.extend(dropped(&at, api));
    send_metrics(address, &bearer, &parts).await;
    let after = add(&server, &bearer, &last, &its_status, 1, &at).await;

    let grown: Vec<f64> = read
        .iter()
        .map(|series| grew(&before, &after, series))
        .collect();
    // 404 by its three kept points' 1, 10 and 100 requests, 500 by none, though a kept point
    // names it second, and none of the nine statuses that the dropped points name; then each
    // kind of failure by one point; then the series of neither by its two points' 1 and 10
    // requests.
    assert_eq!(
        grown,
        [
            111.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 11.0
        ],
        "{read:?}\n{after}"
    );
    // Each series that the collector serves is under the listed client's name, by a status, by a
    // kind of failure, or by neither.
    for line in after.lines().filter(|line| !line.starts_with('#')) {
        let labels = line
            .split_once('{')
            .and_then(|(_, rest)| rest.rsplit_once('}'))
            .map_or("", |(labels, _)| labels);
        let mut names: Vec<&str> = labels
            .split(',')
            .filter_map(|label| label.split_once('='))
            .map(|(name, _)| name)
            .filter(|name| *name != "le")
            .collect();
        names.sort_unstable();
        assert!(
            matches!(
                names.as_slice(),
                ["job"] | ["http_response_status_code", "job"] | ["error_type", "job"]
            ),
            "{line}"
        );
        assert!(line.contains(&format!("job=\"{CLIENT}\"")), "{line}");
        assert!(
            line.starts_with("http_client_request_duration_seconds_"),
            "{line}"
        );
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

/// Registers a user on `deployment`, has them consent to their clients' telemetry, and returns
/// their access token.
fn consenting(deployment: &Deployment) -> String {
    let root = deployment.server.caddy_root();
    let (registered, session) = post(
        root.clone(),
        deployment.url("https", 443, "/v1/users"),
        users::registration("ada@example.com").to_string(),
    );
    assert_eq!(registered, 201, "{session}");
    let session: Value = serde_json::from_str(&session).expect("a session");
    let token = session["access_token"]
        .as_str()
        .expect("a token")
        .to_owned();
    let consented = put_as(
        root,
        deployment.url("https", 443, "/v1/users/me/telemetry-consent"),
        token.clone(),
        json!({ "consented": true }).to_string(),
    );
    assert_eq!(consented, 200);
    token
}

#[tokio::test]
async fn client_telemetry_016_a_deployment_sends_its_managed_backend_its_clients_telemetry() {
    let deployment = Deployment::with_a_managed_backend().await;
    let token = consenting(&deployment);
    let root = deployment.server.caddy_root();
    let url = |path: &str| deployment.url("https", 443, path);
    let (trace, span) = ids();
    let at = deployment.sql("SELECT (extract(epoch FROM now()) * 1000000000)::bigint::text");
    let at = at.trim();
    let points = listed(&[point(&json!([status(200)]), 3, at)]);
    let metrics = json!({ "resourceMetrics": [measured(&resource(Some(CLIENT)), &points)] });

    let (features, _, capabilities) = get_within(
        root.clone(),
        url("/v1/capabilities"),
        Duration::from_secs(10),
    );
    let sent = [
        (
            "/v1/users/me/telemetry/traces",
            spans(Some(CLIENT), &trace, &span, "settings opened", at),
        ),
        (
            "/v1/users/me/telemetry/logs",
            records(Some(CLIENT), &trace, &span, "the settings opened", at),
        ),
        ("/v1/users/me/telemetry/metrics", metrics),
    ]
    .map(|(path, batch)| {
        post_as(
            root.clone(),
            url(path),
            Some(token.clone()),
            batch.to_string(),
        )
    });
    // The collector scrapes the series that it adds up every 15 seconds.
    let received = deployment.server.shell(&format!(
        "timeout 120 sh -c 'until grep resourceSpans /root/backend/out/telemetry.json 2> /dev/null | grep -q {trace} \
         && grep resourceLogs /root/backend/out/telemetry.json | grep -q {trace} \
         && grep resourceMetrics /root/backend/out/telemetry.json | grep {CLIENT} | grep -q http_client_request_duration_seconds; \
         do sleep 2; done'"
    ));

    assert_eq!(features, 200, "{capabilities}");
    let capabilities: Value = serde_json::from_str(&capabilities).expect("the capabilities");
    assert_eq!(capabilities["features"]["client_telemetry"], true);
    for taken in sent {
        assert_eq!(taken, (200, "{}".to_owned()));
    }
    assert!(
        received.status.success(),
        "{}\nThe backend's log:\n{}\nThe collector's log:\n{}",
        lossy(&received),
        deployment
            .server
            .shell_ok("docker logs --tail 10 backend 2>&1"),
        deployment
            .server
            .shell_ok("docker logs --tail 30 deployment-collector-managed-1 2>&1")
    );
}
