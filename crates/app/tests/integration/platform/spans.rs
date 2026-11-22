//! The spans of the database's transactions (PLATFORM-104), as `serve` exports them.

use opentelemetry_proto::tonic::trace::v1::{Span, span::SpanKind};
use platform::telemetry::testing::collector::{Collector, Received, attribute};

use crate::support::{
    Server, TestDatabase,
    events::{self, Event},
    http_exchange, users,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A `serve` on `database` that exports to `collector`.
async fn serve(database: &TestDatabase, collector: &Collector) -> Server {
    database
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint()),
        ])
        .await
}

/// Sends `body` as JSON to `path` of `server`, in the trace `trace`, and returns the response.
async fn post(server: &Server, path: &str, body: &serde_json::Value, trace: &str) -> String {
    let body = body.to_string();
    http_exchange(
        server.address(),
        "POST",
        path,
        &[
            ("traceparent", &format!("00-{trace}-b7ad6b7169203331-01")),
            ("content-type", "application/json"),
            ("content-length", &body.len().to_string()),
        ],
        &body,
    )
    .await
    .expect("a response from the server")
}

/// The spans among `received` whose parent is `parent`.
fn children<'a>(received: &'a Received, parent: &Span) -> Vec<&'a Span> {
    received
        .spans()
        .into_iter()
        .map(|(_, span)| span)
        .filter(|span| span.parent_span_id == parent.span_id)
        .collect()
}

/// The names of `span`'s attributes, sorted.
fn keys(span: &Span) -> Vec<&str> {
    let mut keys: Vec<&str> = span
        .attributes
        .iter()
        .map(|pair| pair.key.as_str())
        .collect();
    keys.sort_unstable();
    keys
}

/// Asserts that `span` is a client span that names Postgres and `database`, and that it began
/// and ended while `parent` lasted.
fn assert_a_client_s_of(span: &Span, database: &TestDatabase, parent: &Span) {
    assert_eq!(span.kind, i32::from(SpanKind::Client), "{span:?}");
    assert_eq!(
        attribute(&span.attributes, "db.system.name").as_deref(),
        Some("postgresql")
    );
    assert_eq!(
        attribute(&span.attributes, "db.namespace").as_deref(),
        Some(database.name())
    );
    assert!(
        parent.start_time_unix_nano <= span.start_time_unix_nano
            && span.end_time_unix_nano <= parent.end_time_unix_nano,
        "{span:?} within {parent:?}"
    );
}

#[tokio::test]
async fn platform_104_a_request_s_unit_of_work_and_its_limits_counts_run_in_child_spans() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = serve(&database, &collector).await;
    let registered = post(
        &server,
        "/v1/users",
        &users::registration("ada@example.com"),
        "0af7651916cd43dd8448eb211c80319c",
    )
    .await;
    let trace = "5bf1a8b1a1e04e7a8c0f1d2e3a4b5c6d";

    let signed_in = post(
        &server,
        "/v1/sessions",
        &serde_json::json!({ "email": "ada@example.com", "password": users::PASSWORD }),
        trace,
    )
    .await;
    let is_the_request_s = |span: &Span| -> bool {
        hex(&span.trace_id) == trace && span.kind == i32::from(SpanKind::Server)
    };
    // The request's span ends after the spans inside it, so its export follows theirs.
    let received = collector
        .wait_for(|received| {
            received
                .spans()
                .iter()
                .any(|(_, span)| is_the_request_s(span))
        })
        .await;

    assert!(registered.starts_with("HTTP/1.1 201"), "{registered}");
    assert!(signed_in.starts_with("HTTP/1.1 201"), "{signed_in}");
    let (_, request) = received
        .spans()
        .into_iter()
        .find(|(_, span)| is_the_request_s(span))
        .unwrap();
    let within = children(&received, request);
    let named = |name: &str| -> Vec<&Span> {
        within
            .iter()
            .copied()
            .filter(|span| span.name == name)
            .collect()
    };
    // The sign-in checks the password inside its unit of work, whose span lasts as long.
    let (checks, transactions) = (named("verify_password"), named("transaction"));
    assert_eq!(checks.len(), 1, "{within:?}");
    assert_eq!(transactions.len(), 1, "{within:?}");
    let (check, transaction) = (checks[0], transactions[0]);
    assert_a_client_s_of(transaction, &database, request);
    assert_eq!(keys(transaction), ["db.namespace", "db.system.name"]);
    assert!(
        transaction.start_time_unix_nano <= check.start_time_unix_nano
            && check.end_time_unix_nano <= transaction.end_time_unix_nano,
        "{check:?} within {transaction:?}"
    );
    let counts = named("INSERT rate_limit_buckets");
    assert!(!counts.is_empty(), "{within:?}");
    for count in counts {
        assert_a_client_s_of(count, &database, request);
        assert_eq!(
            keys(count),
            ["db.namespace", "db.query.summary", "db.system.name"]
        );
        assert_eq!(
            attribute(&count.attributes, "db.query.summary").as_deref(),
            Some("INSERT rate_limit_buckets")
        );
    }
}

#[tokio::test]
async fn platform_104_a_job_s_run_has_child_spans_for_its_unit_of_work_and_its_outcome() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let _server = serve(&database, &collector).await;

    // A sweep is due once `serve` starts: it deletes in a unit of work, and the scheduler then
    // records its next run, in a statement that commits by itself.
    let sweep = "platform.expire_events";
    let received = collector
        .wait_for(|received| received.spans().iter().any(|(_, span)| span.name == sweep))
        .await;

    let (_, run) = received
        .spans()
        .into_iter()
        .find(|(_, span)| span.name == sweep)
        .unwrap();
    let within = children(&received, run);
    let names: Vec<&str> = within.iter().map(|span| span.name.as_str()).collect();
    assert_eq!(names, ["transaction", "UPDATE job_schedules"]);
    for span in &within {
        assert_a_client_s_of(span, &database, run);
    }
    assert_eq!(keys(within[0]), ["db.namespace", "db.system.name"]);
    assert_eq!(
        attribute(&within[1].attributes, "db.query.summary").as_deref(),
        Some("UPDATE job_schedules")
    );
}

#[tokio::test]
async fn platform_104_a_transaction_outside_every_span_opens_no_span() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let _server = serve(&database, &collector).await;
    let user = users::without_password(&database, "u@example.com").await;

    // Outside every span, the scheduler has claimed the sweeps, which are due once `serve`
    // starts, and the hub's task loads the event that commits here. The span of each, had it
    // one, would end before the span of the run or of the delivery that follows it begins, so
    // its export would come no later than theirs.
    database
        .owner_executes(&events::statement(
            &[Event::to_user(&user, "note.made", "{}")],
            true,
        ))
        .await;
    let received = collector
        .wait_for(|received| {
            let arrived = |name: &str| received.spans().iter().any(|(_, span)| span.name == name);
            arrived("deliver event") && arrived("platform.expire_events")
        })
        .await;

    let without_a_parent: Vec<&str> = received
        .spans()
        .into_iter()
        .filter(|(_, span)| attribute(&span.attributes, "db.system.name").is_some())
        .filter(|(_, span)| span.parent_span_id.is_empty())
        .map(|(_, span)| span.name.as_str())
        .collect();
    assert_eq!(without_a_parent, <[&str; 0]>::default());
}
