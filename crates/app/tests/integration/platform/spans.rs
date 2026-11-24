//! The spans of the database's transactions (PLATFORM-104), as `serve` exports them, and, in
//! process, how long a unit of work's span lasts.

use std::{future::Future, time::Duration};

use opentelemetry_proto::tonic::trace::v1::{Span, span::SpanKind};
use platform::{
    jobs::JobContext,
    telemetry::testing::{
        collector::{Collector, attribute},
        spans::OpenSpans,
    },
};
use tracing::{Instrument, instrument::WithSubscriber};

use crate::support::{
    TestDatabase, children,
    events::{self, Event},
    execute_on, is_the_request_s, query_waits_on_a_lock, users,
};

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
    let server = database.exporting_to(&collector, &[]).await;
    let registered = server
        .post_in_trace(
            "/v1/users",
            &users::registration("ada@example.com"),
            "0af7651916cd43dd8448eb211c80319c",
        )
        .await;
    let trace = "5bf1a8b1a1e04e7a8c0f1d2e3a4b5c6d";

    let signed_in = server
        .post_in_trace(
            "/v1/sessions",
            &serde_json::json!({ "email": "ada@example.com", "password": users::PASSWORD }),
            trace,
        )
        .await;
    // The request's span ends after the spans inside it, so its export follows theirs.
    let received = collector
        .wait_for(|received| {
            received
                .spans()
                .iter()
                .any(|(_, span)| is_the_request_s(span, trace))
        })
        .await;

    assert!(registered.starts_with("HTTP/1.1 201"), "{registered}");
    assert!(signed_in.starts_with("HTTP/1.1 201"), "{signed_in}");
    let (_, request) = received
        .spans()
        .into_iter()
        .find(|(_, span)| is_the_request_s(span, trace))
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
    let _server = database.exporting_to(&collector, &[]).await;

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
    let _server = database.exporting_to(&collector, &[]).await;
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

/// Runs `work` inside a span, with `spans` the subscriber of every span that opens in it.
async fn around<T>(spans: &OpenSpans, work: impl Future<Output = T>) -> T {
    async {
        let run = tracing::info_span!("run");
        work.instrument(run).await
    }
    .with_subscriber(spans.dispatch())
    .await
}

/// The advisory lock that a commit ordered with the events takes before it commits
/// (PLATFORM-028): the bytes of `events`.
const POSITION_LOCK: i64 = 0x6576_656e_7473;

#[tokio::test]
async fn platform_104_a_unit_of_work_s_span_lasts_until_it_has_committed() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let spans = OpenSpans::default();
    let mut held = db
        .holding(&format!("SELECT pg_advisory_xact_lock({POSITION_LOCK})"))
        .await;

    // The commit waits for the lock, once the unit of work's statements have run.
    let committing = tokio::spawn({
        let spans = spans.clone();
        async move {
            around(&spans, async {
                let mut uow = database.for_job(&JobContext::for_tests()).await?;
                uow.order_with_events();
                uow.commit().await
            })
            .await
        }
    });
    query_waits_on_a_lock(&db, "pg_advisory_xact_lock").await;
    let while_it_commits = spans.transaction_is_open();
    execute_on(&mut held, "COMMIT").await;
    let committed = committing.await.unwrap();

    assert!(committed.is_ok(), "{committed:?}");
    assert!(while_it_commits);
    assert!(!spans.transaction_is_open());
}

#[tokio::test]
async fn platform_104_a_unit_of_work_s_span_lasts_until_it_is_dropped() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let spans = OpenSpans::default();

    let (ran, while_it_lasts, dropped) = around(&spans, async {
        let mut uow = database.for_job(&JobContext::for_tests()).await.unwrap();
        // A statement of the unit of work's own.
        let ran = uow.wait_for_locks_at_most(Duration::from_secs(1)).await;
        let while_it_lasts = spans.transaction_is_open();
        drop(uow);
        (ran, while_it_lasts, spans.transaction_is_open())
    })
    .await;

    assert!(ran.is_ok(), "{ran:?}");
    assert!(while_it_lasts);
    assert!(!dropped);
}
