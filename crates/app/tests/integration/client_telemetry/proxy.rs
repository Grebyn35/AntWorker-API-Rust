//! The telemetry proxy: the batches of a consenting user's clients, which the API forwards to the
//! collector, here a stand-in (the client_telemetry spec, CLIENT_TELEMETRY-004 to
//! CLIENT_TELEMETRY-010 and CLIENT_TELEMETRY-013), the routes that take them and the relay that
//! forwards them (PLATFORM-090, PLATFORM-091), and the relay's span (OBS-08).

use std::{io::Write, sync::Arc, time::Duration};

use bytes::Bytes;
use flate2::{Compression, write::GzEncoder};
use http::Method;
use opentelemetry_proto::tonic::{
    common::v1::any_value,
    trace::v1::{Span, span::SpanKind},
};
use platform::{
    clock::TestClock,
    telemetry::{OtlpEndpoint, Relay, RelayError, Signal, testing::collector::Collector},
};
use serde_json::json;

use super::{CONSENT, choose, read};
use crate::support::{
    Api, Options, Server, TestDatabase,
    api::{Response, refusal},
    budgets, closed_port, counts,
    upstream::Upstream,
    users::{self, SignedIn},
};

/// The proxy's routes.
const TRACES: &str = "/v1/users/me/telemetry/traces";
const LOGS: &str = "/v1/users/me/telemetry/logs";
const METRICS: &str = "/v1/users/me/telemetry/metrics";

/// The protocol's two media types.
const PROTOBUF: &str = "application/x-protobuf";
const JSON: &str = "application/json";

/// The largest batch, and the least of each budget.
const MEBIBYTE: usize = 1 << 20;

/// The most that a budget's setting takes, which no test's batches reach.
const UNBOUNDED: u32 = 1 << 30;

/// A stand-in collector that takes every batch.
async fn collector() -> Upstream {
    let collector = Upstream::start().await;
    collector.set("POST", "/v1/traces", 200, json!({}));
    collector.set("POST", "/v1/logs", 200, json!({}));
    collector.set("POST", "/v1/metrics", 200, json!({}));
    collector
}

/// An API that forwards to `collector`, within a budget of `user` bytes for each user and of
/// `replica` bytes for the replica, each minute.
fn forwarding(collector: &Upstream, user: u32, replica: u32) -> Options {
    collector
        .collector_options()
        .set("CLIENT_TELEMETRY_USER_BYTES_PER_MINUTE", user)
        .set("CLIENT_TELEMETRY_REPLICA_BYTES_PER_MINUTE", replica)
}

/// A stand-in collector, and an API that forwards to it within budgets that no test's batches
/// reach.
async fn standing() -> (Upstream, Api) {
    let collector = collector().await;
    let api = Api::with(forwarding(&collector, UNBOUNDED, UNBOUNDED)).await;
    (collector, api)
}

/// Registers `email`, and has the user consent to their clients' telemetry.
async fn consenting(api: &Api, email: &str) -> SignedIn {
    let user = users::register(api, email).await;
    assert_eq!(choose(api, &user, true, None).await.status, 200);
    user
}

/// Sends `body` to `path` as `user`, as `media_type`.
async fn send(api: &Api, user: &SignedIn, path: &str, media_type: &str, body: &[u8]) -> Response {
    api.request(Method::POST, path)
        .header("content-type", media_type)
        .bearer(&user.access)
        .body(body)
        .send()
        .await
}

/// Sends `bytes` zeros to the route of spans as `user`, as Protobuf.
async fn send_bytes(api: &Api, user: &SignedIn, bytes: usize) -> Response {
    send(api, user, TRACES, PROTOBUF, &vec![0; bytes]).await
}

/// `bytes` as one member of gzip.
fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// Asserts that `response` is the 429 of the budget `name`, whose window of a minute allows
/// `limit` bytes.
fn assert_refused_by(response: &Response, name: &str, limit: u32) {
    budgets::assert_refused_by(response, name, limit, 60);
}

#[tokio::test]
async fn client_telemetry_004_a_consenting_caller_s_batch_reaches_the_collector_as_it_was_sent() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;
    let spans = [0x0a, 0x03, 0xff, 0x00, 0x7f];
    let records = json!({ "resourceLogs": [{ "scopeLogs": [] }] }).to_string();
    let points = [0x0a, 0x02, 0x12, 0x00];

    let traces = api
        .request(Method::POST, TRACES)
        .header("content-type", PROTOBUF)
        .header("cookie", "theme=dark")
        .bearer(&ada.access)
        .body(spans)
        .send()
        .await;
    let logs = api
        .request(Method::POST, LOGS)
        .header("content-type", "application/json; charset=utf-8")
        .bearer(&ada.access)
        .body(records.clone())
        .send()
        .await;
    let metrics = send(&api, &ada, METRICS, PROTOBUF, &points).await;

    assert_eq!(traces.status, 200);
    assert_eq!(traces.header("content-type"), PROTOBUF);
    assert!(traces.body.is_empty());
    assert_eq!(logs.status, 200);
    assert_eq!(logs.header("content-type"), JSON);
    assert_eq!(logs.body, b"{}");
    assert_eq!(metrics.status, 200);
    assert_eq!(metrics.header("content-type"), PROTOBUF);
    assert!(metrics.body.is_empty());
    let received = collector.received();
    assert_eq!(received.len(), 3);
    let paths: Vec<_> = received
        .iter()
        .map(|request| format!("{} {}", request.method, request.path))
        .collect();
    assert_eq!(
        paths,
        ["POST /v1/traces", "POST /v1/logs", "POST /v1/metrics"]
    );
    assert_eq!(received[0].bytes, spans);
    assert_eq!(received[1].bytes, records.as_bytes());
    assert_eq!(received[2].bytes, points);
    for (request, media_type) in received.iter().zip([PROTOBUF, JSON, PROTOBUF]) {
        let header = |name: &str| {
            request
                .headers
                .iter()
                .find(|(header, _)| header == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("content-type"), Some(media_type));
        assert_eq!(header("authorization"), None);
        assert_eq!(header("cookie"), None);
        assert_eq!(header("content-encoding"), None);
    }
}

#[tokio::test]
async fn client_telemetry_004_a_gzip_batch_reaches_the_collector_inflated() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;
    let spans = b"a batch of spans, a batch of spans, a batch of spans";

    let response = api
        .request(Method::POST, TRACES)
        .header("content-type", PROTOBUF)
        .header("content-encoding", "gzip")
        .bearer(&ada.access)
        .body(gzip(spans))
        .send()
        .await;

    assert_eq!(response.status, 200);
    let received = collector.received();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].bytes, spans);
    assert!(
        received[0]
            .headers
            .iter()
            .all(|(name, _)| name != "content-encoding")
    );
}

#[tokio::test]
async fn client_telemetry_004_a_batch_is_taken_whichever_2xx_the_collector_answers() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;

    for status in [200, 202, 299] {
        collector.set(
            "POST",
            "/v1/traces",
            status,
            json!({ "partialSuccess": {} }),
        );

        let taken = send_bytes(&api, &ada, 8).await;

        assert_eq!(taken.status, 200, "the collector's {status}");
        assert!(taken.body.is_empty());
    }
}

#[tokio::test]
async fn client_telemetry_005_a_caller_who_doesn_t_consent_is_refused_on_each_route() {
    let (collector, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;

    let never_chose = send_bytes(&api, &ada, 8).await;
    let logs = send(&api, &ada, LOGS, JSON, b"{}").await;
    let metrics = send(&api, &ada, METRICS, PROTOBUF, &[0; 8]).await;
    assert_eq!(choose(&api, &ada, true, None).await.status, 200);
    let consenting = send_bytes(&api, &ada, 8).await;
    assert_eq!(choose(&api, &ada, false, None).await.status, 200);
    let withdrew = send_bytes(&api, &ada, 8).await;

    for refused in [&never_chose, &logs, &metrics, &withdrew] {
        assert_eq!(refused.status, 403);
        assert_eq!(refused.json()["code"], "client_telemetry.consent_required");
    }
    assert_eq!(consenting.status, 200);
    assert_eq!(collector.received().len(), 1, "the consenting batch alone");
}

#[tokio::test]
async fn client_telemetry_005_a_batch_without_consent_counts_in_no_budget() {
    let collector = collector().await;
    let whole = u32::try_from(MEBIBYTE).unwrap();
    let api = Api::with(forwarding(&collector, whole, whole)).await;
    let ada = users::register(&api, "ada@example.com").await;

    // Two batches that the budgets couldn't both hold, had either counted.
    let first = send_bytes(&api, &ada, 600 * 1024).await;
    let second = send_bytes(&api, &ada, 600 * 1024).await;
    assert_eq!(choose(&api, &ada, true, None).await.status, 200);
    let consenting = send_bytes(&api, &ada, 600 * 1024).await;

    assert_eq!(first.status, 403);
    assert_eq!(second.status, 403);
    assert_eq!(consenting.status, 200);
}

#[tokio::test]
async fn client_telemetry_005_one_user_s_consent_admits_no_other_user_s_batch() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;

    let hers = send_bytes(&api, &ada, 8).await;
    let other = send_bytes(&api, &grace, 8).await;
    let anonymous = api
        .request(Method::POST, TRACES)
        .header("content-type", PROTOBUF)
        .body([0; 8])
        .send()
        .await;

    assert_eq!(hers.status, 200);
    assert_eq!(other.status, 403);
    assert_eq!(anonymous.status, 401);
    assert_eq!(collector.received().len(), 1);
}

#[tokio::test]
async fn client_telemetry_006_a_body_that_isn_t_a_batch_is_refused_and_counts_in_no_budget() {
    let collector = collector().await;
    let whole = u32::try_from(MEBIBYTE).unwrap();
    let api = Api::with(forwarding(&collector, whole, whole)).await;
    let ada = consenting(&api, "ada@example.com").await;
    let coded = |coding: &str, body: Vec<u8>| {
        api.request(Method::POST, TRACES)
            .header("content-type", PROTOBUF)
            .header("content-encoding", coding)
            .bearer(&ada.access)
            .body(body)
    };

    let other_type = send(&api, &ada, TRACES, "application/octet-stream", b"spans").await;
    let no_metrics = send(&api, &ada, METRICS, "text/csv", b"points").await;
    let other_coding = coded("br", b"spans".to_vec()).send().await;
    let not_gzip = coded("gzip", b"spans".to_vec()).send().await;
    let too_large = send_bytes(&api, &ada, MEBIBYTE + 1).await;
    let inflates_too_large = coded("gzip", gzip(&vec![0; MEBIBYTE + 1])).send().await;
    // The route reads the body before it asks whether its caller consents.
    let grace = users::register(&api, "grace@example.com").await;
    let without_consent = send(&api, &grace, LOGS, "text/plain", b"records").await;
    let too_large_without_consent = send_bytes(&api, &grace, MEBIBYTE + 1).await;
    // The largest batch, which the budgets hold only while nothing else has counted in them.
    let largest = coded("identity", vec![0; MEBIBYTE]).send().await;

    let refused = [
        &other_type,
        &no_metrics,
        &other_coding,
        &not_gzip,
        &without_consent,
    ];
    for refused in refused {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "request.invalid_body");
    }
    for refused in [&too_large, &inflates_too_large, &too_large_without_consent] {
        assert_eq!(refused.status, 413);
        assert_eq!(refused.json()["code"], "request.payload_too_large");
    }
    assert_eq!(largest.status, 200);
    assert_eq!(collector.received().len(), 1, "the largest batch alone");
}

#[tokio::test]
async fn client_telemetry_007_a_user_s_budget_counts_their_batches_on_each_route_and_every_replica()
{
    let collector = collector().await;
    let whole = u32::try_from(MEBIBYTE).unwrap();
    let options = || forwarding(&collector, whole, UNBOUNDED);
    let api = Api::with(options()).await;
    let replica = api.instance(options()).await;
    let ada = consenting(&api, "ada@example.com").await;
    let grace = consenting(&api, "grace@example.com").await;
    let records = vec![b' '; 400 * 1024];

    // Another client of the user's: a session of its own.
    let other_client = users::new_session(&api, "ada@example.com").await;

    let first = send_bytes(&api, &ada, 400 * 1024).await;
    let on_logs = send(&api, &other_client, LOGS, JSON, &records).await;
    let on_metrics = send(&api, &ada, METRICS, PROTOBUF, &vec![0; 100 * 1024]).await;
    let beyond = send_bytes(&replica, &ada, 400 * 1024).await;
    let afterwards = send(&api, &ada, METRICS, PROTOBUF, &[0]).await;
    let other_user = send_bytes(&replica, &grace, 400 * 1024).await;

    assert_eq!(first.status, 200);
    assert_eq!(on_logs.status, 200);
    assert_eq!(on_metrics.status, 200);
    assert_refused_by(&beyond, "telemetry_user", whole);
    // The refused batch stays counted, so the window refuses the user's later batches.
    assert_refused_by(&afterwards, "telemetry_user", whole);
    assert_eq!(other_user.status, 200);
    assert_eq!(collector.received().len(), 4);
}

#[tokio::test]
async fn client_telemetry_007_a_gzip_batch_counts_its_inflated_bytes() {
    let collector = collector().await;
    let whole = u32::try_from(MEBIBYTE).unwrap();
    let api = Api::with(forwarding(&collector, whole, UNBOUNDED)).await;
    let ada = consenting(&api, "ada@example.com").await;
    let batch = gzip(&vec![0; 600 * 1024]);
    assert!(batch.len() < 1024, "600 KiB of zeros are few bytes of gzip");
    let gzipped = || {
        api.request(Method::POST, TRACES)
            .header("content-type", PROTOBUF)
            .header("content-encoding", "gzip")
            .bearer(&ada.access)
            .body(batch.clone())
    };

    let first = gzipped().send().await;
    let second = gzipped().send().await;

    assert_eq!(first.status, 200);
    assert_refused_by(&second, "telemetry_user", whole);
}

#[tokio::test]
async fn client_telemetry_008_a_replica_s_budget_counts_every_user_s_batches_that_it_serves() {
    let collector = collector().await;
    let whole = u32::try_from(MEBIBYTE).unwrap();
    let options = || forwarding(&collector, whole, whole);
    let api = Api::with(options()).await;
    let replica = api.instance(options()).await;
    let ada = consenting(&api, "ada@example.com").await;
    let grace = consenting(&api, "grace@example.com").await;

    let first = send_bytes(&api, &ada, 600 * 1024).await;
    let beyond = send_bytes(&api, &grace, 600 * 1024).await;
    let afterwards = send_bytes(&api, &ada, 1).await;
    // The other replica's budget is its own, and holds a batch; the user's budget, which every
    // replica shares, counted the batch that this replica's refused.
    let elsewhere = send_bytes(&replica, &grace, 600 * 1024).await;
    let in_its_own = send_bytes(&replica, &ada, 100 * 1024).await;

    assert_eq!(first.status, 200);
    assert_refused_by(&beyond, "telemetry_replica", whole);
    // The refused batch stays counted, so the window refuses the replica's later batches.
    assert_refused_by(&afterwards, "telemetry_replica", whole);
    assert_refused_by(&elsewhere, "telemetry_user", whole);
    assert_eq!(in_its_own.status, 200);
    assert_eq!(collector.received().len(), 2);
}

#[tokio::test]
async fn client_telemetry_008_a_replica_s_budget_counts_the_batches_of_logs_and_of_metrics() {
    for (first, second) in [(LOGS, METRICS), (METRICS, LOGS)] {
        let collector = collector().await;
        let whole = u32::try_from(MEBIBYTE).unwrap();
        let api = Api::with(forwarding(&collector, whole, whole)).await;
        let ada = consenting(&api, "ada@example.com").await;
        let grace = consenting(&api, "grace@example.com").await;
        let batch = vec![0; 600 * 1024];

        let taken = send(&api, &ada, first, PROTOBUF, &batch).await;
        let beyond = send(&api, &grace, second, PROTOBUF, &batch).await;
        let afterwards = send(&api, &ada, first, PROTOBUF, &[0]).await;

        assert_eq!(taken.status, 200, "{first}");
        assert_refused_by(&beyond, "telemetry_replica", whole);
        // The refused batch stays counted, so the window refuses the replica's later batches.
        assert_refused_by(&afterwards, "telemetry_replica", whole);
        let received: Vec<String> = collector
            .received()
            .into_iter()
            .map(|request| request.path)
            .collect();
        assert_eq!(received.len(), 1, "{first} then {second}: {received:?}");
    }
}

#[tokio::test]
async fn client_telemetry_008_by_default_a_replica_s_budget_holds_four_users_full_budgets() {
    let collector = collector().await;
    let api = Api::with(collector.collector_options()).await;
    let mut users = Vec::new();
    for name in ["ada", "grace", "edsger", "barbara", "alan"] {
        users.push(consenting(&api, &format!("{name}@example.com")).await);
    }

    // A user's whole budget, by default the largest batch, from each of four users, between
    // which the first sends one byte more.
    let first = send_bytes(&api, &users[0], MEBIBYTE).await;
    let her_next = send_bytes(&api, &users[0], 1).await;
    let mut others = Vec::new();
    for user in &users[1..4] {
        others.push(send_bytes(&api, user, MEBIBYTE).await);
    }
    let fifth = send_bytes(&api, &users[4], 1).await;

    assert_eq!(first.status, 200);
    assert_refused_by(&her_next, "telemetry_user", 1 << 20);
    // What her own budget refused counted nothing in the replica's, which the four fill.
    for other in &others {
        assert_eq!(other.status, 200);
    }
    assert_refused_by(&fifth, "telemetry_replica", 4 << 20);
    assert_eq!(collector.received().len(), 4);
}

#[tokio::test]
async fn client_telemetry_008_without_its_setting_a_replica_s_budget_is_four_times_a_user_s() {
    let collector = collector().await;
    let twice = 2 * u32::try_from(MEBIBYTE).unwrap();
    let options = collector
        .collector_options()
        .set("CLIENT_TELEMETRY_USER_BYTES_PER_MINUTE", twice);
    let api = Api::with(options).await;
    let mut users = Vec::new();
    for name in ["ada", "grace", "edsger", "barbara", "alan"] {
        users.push(consenting(&api, &format!("{name}@example.com")).await);
    }

    // The whole budget of each of four users, two of the largest batches.
    for user in &users[..4] {
        for _ in 0..2 {
            assert_eq!(send_bytes(&api, user, MEBIBYTE).await.status, 200);
        }
    }
    let fifth = send_bytes(&api, &users[4], 1).await;

    assert_refused_by(&fifth, "telemetry_replica", 4 * twice);
}

/// Each route that takes a batch, and the collector's path that its batches go to.
const ROUTES: [(&str, &str); 3] = [
    (TRACES, "/v1/traces"),
    (LOGS, "/v1/logs"),
    (METRICS, "/v1/metrics"),
];

#[tokio::test]
async fn client_telemetry_009_a_batch_that_the_collector_refuses_answers_400() {
    for (route, at) in ROUTES {
        let collector = collector().await;
        let whole = u32::try_from(MEBIBYTE).unwrap();
        let api = Api::with(forwarding(&collector, whole, UNBOUNDED)).await;
        let ada = consenting(&api, "ada@example.com").await;
        let batch = vec![0; 300 * 1024];

        for status in [400, 413, 415] {
            collector.set("POST", at, status, json!({ "code": 3 }));

            let refused = send(&api, &ada, route, PROTOBUF, &batch).await;

            assert_eq!(refused.status, 400, "{route}: the collector's {status}");
            assert_eq!(refused.json()["code"], "client_telemetry.batch_refused");
        }
        // The three refused batches counted in the user's budget, which holds no fourth.
        collector.set("POST", at, 200, json!({}));
        let fourth = send(&api, &ada, route, PROTOBUF, &batch).await;
        assert_refused_by(&fourth, "telemetry_user", whole);
    }
}

#[tokio::test]
async fn client_telemetry_009_a_collector_that_doesn_t_take_a_batch_answers_503() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;

    for (route, at) in ROUTES {
        for status in [302, 404, 405, 429, 500, 503] {
            collector.set("POST", at, status, json!({}));

            let unavailable = send(&api, &ada, route, PROTOBUF, &[0; 8]).await;

            assert_eq!(unavailable.status, 503, "{route}: the collector's {status}");
            assert_eq!(
                unavailable.json()["code"],
                "client_telemetry.collector_unavailable"
            );
            assert_eq!(unavailable.header("retry-after"), "1");
        }
    }
}

#[tokio::test]
async fn client_telemetry_009_a_collector_that_doesn_t_answer_within_5_seconds_answers_503() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;
    collector.hang("POST", "/v1/traces");
    let clock = api.clock();
    let call = {
        let (api, ada) = (api.clone(), ada.access.clone());
        tokio::spawn(async move {
            api.request(Method::POST, TRACES)
                .header("content-type", PROTOBUF)
                .bearer(&ada)
                .body([0; 8])
                .send()
                .await
        })
    };

    // The relay's deadline waits once its request is sent, so the clock moves only then, once,
    // by the 5 seconds.
    while collector.received().is_empty() {
        tokio::task::yield_now().await;
    }
    clock.sleeping(Duration::from_secs(5)).await;
    clock.advance(Duration::from_secs(5));

    let response = call.await.unwrap();
    assert_eq!(response.status, 503);
    assert_eq!(
        response.json()["code"],
        "client_telemetry.collector_unavailable"
    );
}

#[tokio::test]
async fn client_telemetry_009_a_collector_that_no_connection_reaches_answers_503() {
    let closed = closed_port();
    let endpoint = format!("http://{}", closed.address());
    let api = Api::with(Options::default().set("CLIENT_TELEMETRY_ENDPOINT", endpoint)).await;
    let ada = consenting(&api, "ada@example.com").await;

    let response = send_bytes(&api, &ada, 8).await;

    assert_eq!(response.status, 503);
    assert_eq!(
        response.json()["code"],
        "client_telemetry.collector_unavailable"
    );
}

#[tokio::test]
async fn client_telemetry_010_without_the_setting_the_routes_answer_503_and_the_feature_is_off() {
    let api = Api::new().await;
    let ada = consenting(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;

    for path in [TRACES, LOGS, METRICS] {
        for user in [&ada, &grace] {
            let response = send(&api, user, path, PROTOBUF, &[0; 8]).await;

            assert_eq!(response.status, 503);
            assert_eq!(response.json()["code"], "request.feature_disabled");
            // Only a deploy turns the feature on, so a client waits long before it asks again.
            assert_eq!(response.header("retry-after"), "3600");
        }
        let anonymous = api
            .request(Method::POST, path)
            .header("content-type", PROTOBUF)
            .body([0; 8])
            .send()
            .await;
        assert_eq!(anonymous.status, 401);
        // The route reads the body first, so one that isn't a batch is refused as one.
        let no_batch = send(&api, &ada, path, "text/plain", b"spans").await;
        assert_eq!(no_batch.status, 400);
    }
    let capabilities = api.get("/v1/capabilities").send().await.json();
    assert_eq!(capabilities["features"]["client_telemetry"], false);
    // The consent's routes serve all the same: the user chose above, and reads the choice.
    let (status, consent, _) = read(&api, &ada).await;
    assert_eq!((status, &consent["consented"]), (200, &json!(true)));
    assert_eq!(choose(&api, &ada, false, None).await.status, 200);
}

#[test]
fn client_telemetry_010_serve_refuses_an_endpoint_that_isn_t_an_http_or_https_url() {
    for endpoint in [
        "collector:4319",
        "ftp://collector:4319",
        "http://:4319",
        "http://user@collector:4319",
        "http://:secret@collector:4319",
        "http://collector:4319?token=secret",
        "http://collector:4319#traces",
    ] {
        let refusal = refusal(&Options::default().set("CLIENT_TELEMETRY_ENDPOINT", endpoint));

        assert_eq!(
            refusal,
            "CLIENT_TELEMETRY_ENDPOINT must be an http or https URL with a host, and no user, \
             password, query or fragment",
            "{endpoint}"
        );
    }
}

#[tokio::test]
async fn client_telemetry_010_with_the_setting_the_feature_is_on() {
    let (_collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;

    let capabilities = api.get("/v1/capabilities").send().await.json();
    let (status, consent, _) = read(&api, &ada).await;

    assert_eq!(capabilities["features"]["client_telemetry"], true);
    assert_eq!((status, &consent["consented"]), (200, &json!(true)));
}

/// A `serve` of `api`'s database that forwards batches to `collector` within budgets that no
/// test's batches reach, and exports its own telemetry to `exported`.
async fn forwarding_serve(api: &Api, collector: &Upstream, exported: &Collector) -> Server {
    let options = forwarding(collector, UNBOUNDED, UNBOUNDED);
    let endpoint = exported.endpoint();
    let mut env = options.env();
    env.extend([
        ("ENVIRONMENT", "test"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint),
    ]);
    api.database().serve(&env).await
}

/// Sends `body` to `path` of `server` as `user`, with `headers`, such as its `Content-Type`, and
/// gives the answer's status.
async fn send_to(
    server: &Server,
    user: &SignedIn,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> u16 {
    let bearer = format!("Bearer {}", user.access);
    let mut headers = headers.to_vec();
    headers.push(("authorization", &bearer));
    server.send("POST", path, &headers, body).await.0
}

/// The bytes that `exported` received the count of, from a `serve` that has stopped, by the
/// signal of their batches, such as `traces 1000` (TEST-07).
fn forwarded_counted(exported: &Collector) -> Vec<String> {
    counts::of(
        &exported.received(),
        "client_telemetry.forwarded",
        "client_telemetry.signal",
    )
}

#[tokio::test]
async fn client_telemetry_013_the_bytes_of_a_batch_that_the_collector_took_are_counted_by_signal() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;
    let exported = Collector::start().await.unwrap();
    let server = forwarding_serve(&api, &collector, &exported).await;
    let protobuf = [("content-type", PROTOBUF)];

    let spans = send_to(&server, &ada, TRACES, &protobuf, &[0; 1_000]).await;
    // 600,000 zeros, which gzip sends as few bytes: the count is of the bytes inflated.
    let gzipped = [("content-type", PROTOBUF), ("content-encoding", "gzip")];
    let records = send_to(&server, &ada, LOGS, &gzipped, &gzip(&vec![0; 600_000])).await;
    let points = send_to(&server, &ada, METRICS, &protobuf, &[0; 20_000]).await;
    server.stop().await;

    assert_eq!([spans, records, points], [200; 3]);
    assert_eq!(
        forwarded_counted(&exported),
        ["logs 600000", "metrics 20000", "traces 1000"]
    );
}

#[tokio::test]
async fn client_telemetry_013_a_batch_that_the_collector_refused_or_didn_t_take_counts_no_bytes() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;
    let exported = Collector::start().await.unwrap();
    let server = forwarding_serve(&api, &collector, &exported).await;
    let protobuf = [("content-type", PROTOBUF)];

    collector.set("POST", "/v1/metrics", 400, json!({}));
    let refused = send_to(&server, &ada, METRICS, &protobuf, &[0; 1_000]).await;
    collector.set("POST", "/v1/metrics", 503, json!({}));
    let untaken = send_to(&server, &ada, METRICS, &protobuf, &[0; 1_000]).await;
    server.stop().await;

    assert_eq!((refused, untaken), (400, 503));
    assert!(forwarded_counted(&exported).is_empty());
}

#[tokio::test]
async fn platform_090_a_batch_s_route_has_no_body_limit_of_its_own() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;

    // More than the 64 KiB that a route group lets a body hold.
    let taken = send_bytes(&api, &ada, 65 * 1024).await;

    assert_eq!(taken.status, 200);
    assert_eq!(collector.received()[0].bytes.len(), 65 * 1024);
}

#[tokio::test]
async fn platform_091_the_relay_follows_no_redirect() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;
    collector.redirect();
    collector.set("POST", "/elsewhere", 200, json!({}));

    let response = send_bytes(&api, &ada, 8).await;

    assert_eq!(response.status, 503);
    assert_eq!(
        response.json()["code"],
        "client_telemetry.collector_unavailable"
    );
    let paths: Vec<_> = collector
        .received()
        .into_iter()
        .map(|request| request.path)
        .collect();
    assert_eq!(paths, ["/v1/traces"]);
}

#[tokio::test]
async fn platform_090_a_batch_s_route_has_the_route_groups_timeout() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;
    collector.hang("POST", "/v1/traces");
    let request = tokio::spawn({
        let (api, ada) = (api.clone(), ada.access.clone());
        async move {
            api.request(Method::POST, TRACES)
                .header("content-type", PROTOBUF)
                .bearer(&ada)
                .body([0; 8])
                .send()
                .await
        }
    });
    while collector.received().is_empty() {
        tokio::task::yield_now().await;
    }
    // Paused once the request waits for the collector, whose deadline is on the API's clock,
    // which the test doesn't move, then moved on by a route group's 10 seconds, past the route's
    // timeout, which started before the pause. The test yields, rather than waiting, so the
    // runtime moves the clock no further: under a longer timeout, the request would wait for the
    // collector, which never answers.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    while !request.is_finished() {
        tokio::task::yield_now().await;
    }

    let response = request.await.unwrap();

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "request.timed_out");
}

#[tokio::test]
async fn platform_091_the_relay_s_request_holds_the_batch_and_its_media_type_alone() {
    let (collector, api) = standing().await;
    let ada = consenting(&api, "ada@example.com").await;

    let response = api
        .request(Method::POST, LOGS)
        .header("content-type", JSON)
        .header("x-request-id", "a client's header")
        .header(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        )
        .bearer(&ada.access)
        .body("{}")
        .send()
        .await;

    assert_eq!(response.status, 200);
    let received = collector.received();
    let mut names: Vec<&str> = received[0]
        .headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["accept", "content-length", "content-type", "host"]);
}

/// The relay that the API builds for `endpoint`, whose waits `clock` measures.
fn relay(endpoint: &OtlpEndpoint, clock: &Arc<TestClock>) -> Relay {
    Relay::new(endpoint, clock.clone()).unwrap()
}

/// The batch that the tests of the relay forward.
const BATCH: &[u8] = b"a batch";

/// Each signal, the collector's path of its batches, and a media type of the protocol's.
const SIGNALS: [(Signal, &str, &str); 3] = [
    (Signal::Traces, "/v1/traces", PROTOBUF),
    (Signal::Logs, "/v1/logs", JSON),
    (Signal::Metrics, "/v1/metrics", PROTOBUF),
];

/// What `relay` gives for a batch of spans once the collector answers it `status`.
async fn forwarded_at(collector: &Upstream, relay: &Relay, status: u16) -> Result<(), RelayError> {
    collector.set("POST", "/v1/traces", status, json!({}));
    relay
        .forward(Signal::Traces, PROTOBUF, Bytes::from_static(BATCH))
        .await
}

#[tokio::test]
async fn platform_091_forward_posts_the_batch_to_its_signal_s_path_and_a_2xx_takes_it() {
    let collector = Upstream::start().await;
    for ((_, path, _), status) in SIGNALS.iter().zip([200, 202, 299]) {
        collector.set("POST", path, status, json!({}));
    }
    let relay = relay(&collector.collector_endpoint(), &Arc::default());

    for (signal, path, media_type) in SIGNALS {
        let forwarded = relay
            .forward(signal, media_type, Bytes::from_static(BATCH))
            .await;

        assert!(forwarded.is_ok(), "{path}: {forwarded:?}");
    }
    let received: Vec<_> = collector
        .received()
        .into_iter()
        .map(|request| {
            let media_type = request
                .headers
                .iter()
                .find(|(name, _)| name == "content-type")
                .map(|(_, value)| value.clone());
            (request.method, request.path, media_type, request.bytes)
        })
        .collect();
    let sent: Vec<_> = SIGNALS
        .iter()
        .map(|(_, path, media_type)| {
            let media_type = Some((*media_type).to_owned());
            (
                "POST".to_owned(),
                (*path).to_owned(),
                media_type,
                BATCH.to_vec(),
            )
        })
        .collect();
    assert_eq!(received, sent);
}

#[tokio::test]
async fn platform_091_forward_fails_as_refused_when_the_collector_answers_a_4xx_but_404_405_and_429()
 {
    let collector = Upstream::start().await;
    let relay = relay(&collector.collector_endpoint(), &Arc::default());

    for status in [400, 401, 403, 406, 413, 415, 428, 430, 499] {
        let forwarded = forwarded_at(&collector, &relay, status).await;

        assert!(
            matches!(forwarded, Err(RelayError::Refused)),
            "{status}: {forwarded:?}"
        );
    }
}

#[tokio::test]
async fn platform_091_forward_fails_as_unavailable_when_the_collector_answers_anything_else() {
    let collector = Upstream::start().await;
    let relay = relay(&collector.collector_endpoint(), &Arc::default());

    for status in [300, 302, 404, 405, 429, 500, 503, 599] {
        let forwarded = forwarded_at(&collector, &relay, status).await;

        assert!(
            matches!(forwarded, Err(RelayError::Unavailable(_))),
            "{status}: {forwarded:?}"
        );
    }
}

#[tokio::test]
async fn platform_091_forward_fails_as_unavailable_when_no_connection_opens() {
    let closed = closed_port();
    let endpoint = format!("http://{}", closed.address()).parse().unwrap();

    let forwarded = relay(&endpoint, &Arc::default())
        .forward(Signal::Traces, PROTOBUF, Bytes::from_static(BATCH))
        .await;

    assert!(
        matches!(forwarded, Err(RelayError::Unavailable(_))),
        "{forwarded:?}"
    );
}

#[tokio::test]
async fn platform_091_forward_fails_as_unavailable_when_no_answer_comes_within_5_seconds() {
    let collector = Upstream::start().await;
    collector.hang("POST", "/v1/traces");
    let clock = Arc::new(TestClock::default());
    let forwarding = {
        let relay = relay(&collector.collector_endpoint(), &clock);
        tokio::spawn(async move {
            relay
                .forward(Signal::Traces, PROTOBUF, Bytes::from_static(BATCH))
                .await
        })
    };

    // The relay's deadline waits once its request is sent, so the clock moves only then, once,
    // by the 5 seconds, for which `sleeping` waits: a deadline of another length never comes.
    while collector.received().is_empty() {
        tokio::task::yield_now().await;
    }
    clock.sleeping(Duration::from_secs(5)).await;
    clock.advance(Duration::from_secs(5));

    let forwarded = forwarding.await.unwrap();
    assert!(
        matches!(forwarded, Err(RelayError::Unavailable(_))),
        "{forwarded:?}"
    );
}

/// Whether `span` is the client span of the relay's `POST` of a batch.
fn is_the_relay_s(span: &Span) -> bool {
    span.kind == i32::from(SpanKind::Client) && span.name == "POST"
}

#[tokio::test]
async fn obs_08_the_relay_s_span_exports_the_collector_s_status_as_a_number() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let endpoint = collector.endpoint();
    // Without email, an account is served without proving its address (PLATFORM-022), and the
    // stand-in collector takes the batches too.
    let server = database
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("EMAIL", "off"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint),
            ("CLIENT_TELEMETRY_ENDPOINT", &endpoint),
        ])
        .await;
    let registration = users::registration("ada@example.com").to_string();
    let json_body = [("content-type", "application/json")];
    let (registered, session) = server
        .send("POST", "/v1/users", &json_body, registration.as_bytes())
        .await;
    let bearer = format!("Bearer {}", session["access_token"].as_str().unwrap());
    let consent = json!({ "consented": true }).to_string();
    let signed = [
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ];
    let (consented, _) = server
        .send("PUT", CONSENT, &signed, consent.as_bytes())
        .await;

    // A batch that holds one empty resource of spans.
    let batch = [
        ("authorization", bearer.as_str()),
        ("content-type", PROTOBUF),
    ];
    let (sent, _) = server.send("POST", TRACES, &batch, b"\n\0").await;
    let received = collector
        .wait_for(|received| {
            received
                .spans()
                .iter()
                .any(|(_, span)| is_the_relay_s(span))
        })
        .await;

    assert_eq!((registered, consented, sent), (201, 200, 200));
    let (_, relay) = received
        .spans()
        .into_iter()
        .find(|(_, span)| is_the_relay_s(span))
        .unwrap();
    let status = relay
        .attributes
        .iter()
        .find(|pair| pair.key == "http.response.status_code")
        .and_then(|pair| pair.value.as_ref()?.value.clone());
    assert_eq!(status, Some(any_value::Value::IntValue(200)));
}
