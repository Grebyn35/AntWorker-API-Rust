//! Idempotency keys (the platform spec's "Idempotency"), and publishing events in a unit of work
//! (the platform spec's "Events and the real-time hub"), through the tests' keyed route and
//! their announcements, so they hold whichever of the template's parts a fork keeps.

use serde_json::{Value, json};

use platform::{errors::PlatformError, idempotency::Claimed, jobs::JobContext, telemetry::testing};

use crate::support::{
    TestDatabase,
    announcements::{self, Note, published::NOTED},
    execute_on,
    fixtures::{self, send_keyed},
    users, workspaces,
};

/// The ID of no workspace.
const NO_WORKSPACE: &str = "018f4e6a-7c7b-7d5e-9c1a-3b2f6d0e4a11";

#[tokio::test]
async fn platform_036_a_retry_with_the_key_answers_the_first_result_and_runs_nothing() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let body = json!({ "value": "a" });

    let first = send_keyed(&api, &ada, &body, Some("\"k-1\"")).await;
    let retry = send_keyed(&api, &ada, &body, Some("\"k-1\"")).await;
    let unkeyed = send_keyed(&api, &ada, &body, None).await;

    assert_eq!(first.json(), json!({ "value": "a", "run": 1 }));
    assert_eq!(retry.json(), first.json());
    assert_eq!(unkeyed.json()["run"], 2);
    let records = api
        .database()
        .owner_reads("SELECT key || ' ' || (result->>'run') FROM idempotency_records")
        .await;
    assert_eq!(records, ["k-1 1"]);
}

#[tokio::test]
async fn platform_036_a_claim_that_follows_another_statement_of_its_unit_of_work_fails() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let user = platform::types::UserId::from_uuid(ada.user_id.parse().unwrap());
    let client = crate::support::client();
    let mut late = api.pool().begin(&client, None).await.unwrap();
    let mut first = api.pool().begin(&client, None).await.unwrap();
    late.publish_to_user(&NOTED, user, &Note { label: None })
        .await
        .unwrap();

    let refused = late.claim::<Value>(user, None).await;
    let fresh = first.claim::<Value>(user, None).await;

    assert!(
        matches!(refused, Err(PlatformError::Internal(_))),
        "a claim ran after another statement"
    );
    assert!(matches!(fresh, Ok(Claimed::Fresh)));
}

#[tokio::test]
async fn platform_036_a_claim_of_a_key_that_follows_another_statement_fails() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let body = json!({ "value": "a", "late": true });

    let late = send_keyed(&api, &ada, &body, Some("\"k-1\"")).await;

    assert_eq!(late.status, 500);
    assert_eq!(late.json()["code"], "request.internal_error");
}

#[tokio::test]
async fn platform_036_the_key_with_another_request_answers_422() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    send_keyed(&api, &ada, &json!({ "value": "a" }), Some("\"k-1\"")).await;

    let reused = send_keyed(&api, &ada, &json!({ "value": "b" }), Some("\"k-1\"")).await;

    assert_eq!(reused.status, 422);
    assert_eq!(reused.json()["code"], "request.idempotency_key_reused");
    assert_eq!(
        reused.json()["detail"],
        "The key was sent before with another request: send a new key for this one."
    );
}

#[tokio::test]
async fn platform_036_a_key_whose_first_request_hasn_t_committed_answers_409_then_runs() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let mut held = db.owner_connection().await;
    execute_on(&mut held, "BEGIN").await;
    execute_on(
        &mut held,
        &format!(
            "INSERT INTO idempotency_records (user_id, key, fingerprint) \
             VALUES ('{}', 'k-1', '\\x00')",
            ada.user_id
        ),
    )
    .await;

    let waiting = send_keyed(&api, &ada, &json!({ "value": "a" }), Some("\"k-1\"")).await;
    execute_on(&mut held, "ROLLBACK").await;
    let retry = send_keyed(&api, &ada, &json!({ "value": "a" }), Some("\"k-1\"")).await;

    assert_eq!(waiting.status, 409);
    assert_eq!(waiting.json()["code"], "request.idempotency_key_in_flight");
    assert_eq!(
        waiting.json()["detail"],
        "The key's first request is still running: retry later."
    );
    assert_eq!(retry.status, 200);
    assert_eq!(retry.json()["run"], 1);
}

#[tokio::test]
async fn platform_036_a_claim_whose_insert_fails_otherwise_than_by_giving_up_its_wait_answers_500()
{
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    api.database()
        .owner_executes("REVOKE INSERT ON idempotency_records FROM app")
        .await;

    let refused = send_keyed(&api, &ada, &json!({ "value": "a" }), Some("\"k-1\"")).await;

    assert_eq!(refused.status, 500);
    assert_eq!(refused.json()["code"], "request.internal_error");
}

#[tokio::test]
async fn platform_036_an_operation_that_rolled_back_leaves_no_record_so_its_retry_runs() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;

    let failed = send_keyed(
        &api,
        &ada,
        &json!({ "value": "a", "fail": true }),
        Some("\"k-1\""),
    )
    .await;
    let retry = send_keyed(
        &api,
        &ada,
        &json!({ "value": "a", "fail": true }),
        Some("\"k-1\""),
    )
    .await;

    assert_eq!(failed.status, 500);
    assert_eq!(retry.status, 500, "a retry runs the operation again");
    let records = api
        .database()
        .owner_reads("SELECT count(*)::text FROM idempotency_records")
        .await;
    assert_eq!(records, ["0"]);
}

#[tokio::test]
async fn platform_036_a_record_older_than_24_hours_counts_as_absent() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    send_keyed(&api, &ada, &json!({ "value": "a" }), Some("\"k-1\"")).await;
    api.database()
        .owner_executes("UPDATE idempotency_records SET created_at = now() - interval '25 hours'")
        .await;

    let again = send_keyed(&api, &ada, &json!({ "value": "b" }), Some("\"k-1\"")).await;

    assert_eq!(again.status, 200);
    assert_eq!(again.json(), json!({ "value": "b", "run": 2 }));
}

/// How many claims of a key `platform.idempotent_requests` has counted as `outcome`, in every
/// test that runs alongside.
fn claims(outcome: &str) -> f64 {
    testing::recorded("platform.idempotent_requests")
        .into_iter()
        .filter(|point| {
            point.labels.get("idempotency.outcome").map(String::as_str) == Some(outcome)
        })
        .map(|point| point.value)
        .sum()
}

#[tokio::test]
async fn platform_036_a_claim_of_a_key_is_counted_by_how_it_ended() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let outcomes = ["new", "replayed", "reused", "in_flight"];
    let before = outcomes.map(claims);
    let mut held = api.database().owner_connection().await;
    execute_on(&mut held, "BEGIN").await;
    execute_on(
        &mut held,
        &format!(
            "INSERT INTO idempotency_records (user_id, key, fingerprint) \
             VALUES ('{}', 'k-2', '\\x00')",
            ada.user_id
        ),
    )
    .await;

    // Each outcome is claimed a number of times of its own, so a count under another outcome's
    // label leaves a label short of its number: two new keys, four replays and three reuses of
    // the first, and one claim of the key that the held transaction inserted, which waits for it.
    let (a, b) = (json!({ "value": "a" }), json!({ "value": "b" }));
    let mut sent = vec![(&a, "\"k-1\""), (&a, "\"k-3\"")];
    sent.extend([(&a, "\"k-1\""); 4]);
    sent.extend([(&b, "\"k-1\""); 3]);
    sent.push((&a, "\"k-2\""));
    let mut statuses = Vec::new();
    for (body, key) in sent {
        statuses.push(send_keyed(&api, &ada, body, Some(key)).await.status);
    }
    execute_on(&mut held, "ROLLBACK").await;

    assert_eq!(statuses, [200, 200, 200, 200, 200, 200, 422, 422, 422, 409]);
    // The keyed route is a fixture of the tests' own, which no `serve` has, and a fork may leave
    // out every route of the API's own that takes a key, so this file's tests send none. Other
    // tests claim keys too, so each count grows by at least this test's own (TEST-07).
    let after = outcomes.map(claims);
    let own = [2.0, 4.0, 3.0, 1.0];
    for (((outcome, before), after), own) in outcomes.iter().zip(before).zip(after).zip(own) {
        assert!(after >= before + own, "{outcome}: {after} after {before}");
    }
}

#[tokio::test]
async fn obs_04_a_stored_result_that_doesn_t_read_as_its_type_fails_and_logs_none_of_it() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let body = json!({ "value": "a" });
    send_keyed(&api, &ada, &body, Some("\"k-1\"")).await;
    // A string where the result's type reads a number.
    api.database()
        .owner_executes(
            "UPDATE idempotency_records SET result = jsonb_set(result, '{run}', '\"grace@example.com\"')",
        )
        .await;

    let retry = send_keyed(&api, &ada, &body, Some("\"k-1\"")).await;

    assert_eq!(retry.status, 500);
    let lines = testing::logged(retry.trace_id());
    assert!(
        lines.iter().any(|line| line["level"] == "ERROR"),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .all(|line| !line.to_string().contains("grace@example.com")),
        "{lines:#?}"
    );
}

#[tokio::test]
async fn platform_037_a_malformed_key_answers_400_and_keys_are_per_user() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let body = json!({ "value": "a" });

    let unquoted = send_keyed(&api, &ada, &body, Some("k-1")).await;
    let too_long = send_keyed(&api, &ada, &body, Some(&format!("\"{}\"", "k".repeat(256)))).await;
    let ada_first = send_keyed(&api, &ada, &body, Some("\"k-1\"")).await;
    let grace_same_key = send_keyed(&api, &grace, &body, Some("\"k-1\"")).await;

    for refused in [&unquoted, &too_long] {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "request.invalid_idempotency_key");
        assert_eq!(
            refused.json()["detail"],
            "Send Idempotency-Key as a quoted string of 1 to 255 characters."
        );
    }
    assert_eq!(ada_first.json()["run"], 1);
    assert_eq!(grace_same_key.json()["run"], 2);
}

#[tokio::test]
async fn platform_026_a_published_event_exists_only_if_its_unit_of_work_commits() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    // The registration's session published its start (AUTH-046).
    api.database().owner_executes("DELETE FROM events").await;

    send_keyed(&api, &ada, &json!({ "value": "a", "fail": true }), None).await;
    let rolled_back = api.database().owner_reads("SELECT name FROM events").await;
    send_keyed(&api, &ada, &json!({ "value": "a" }), None).await;
    let committed = api
        .database()
        .owner_reads("SELECT name || ' ' || scope || ' ' || user_id FROM events")
        .await;

    assert!(rolled_back.is_empty(), "{rolled_back:?}");
    assert_eq!(committed, [format!("keyed.ran user {}", ada.user_id)]);
}

#[tokio::test]
async fn platform_098_a_publication_in_a_workspace_that_is_gone_publishes_nothing_and_goes_on() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let acme = workspaces::arrange(&db, "Acme", &[]).await;
    let mut uow = database.for_job(&JobContext::for_tests()).await.unwrap();

    let in_none = announcements::post_unless_gone(&mut uow, NO_WORKSPACE).await;
    // The unit of work goes on: its next event is published, and it commits.
    let in_acme = announcements::post_unless_gone(&mut uow, &acme).await;
    uow.commit().await.unwrap();

    assert!(matches!(in_none, Ok(false)), "{in_none:?}");
    assert!(matches!(in_acme, Ok(true)), "{in_acme:?}");
    let events = db
        .owner_reads("SELECT name || ' ' || workspace_id FROM events")
        .await;
    assert_eq!(events, [format!("announcement.posted {acme}")]);
}

#[tokio::test]
async fn platform_098_a_publication_that_fails_otherwise_fails_as_any_other_does() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let acme = workspaces::arrange(&db, "Acme", &[]).await;
    // A snapshot job's unit of work publishes no workspace event (PLATFORM-085).
    let mut snapshot = database
        .for_snapshot_job(&JobContext::for_tests())
        .await
        .unwrap();

    let refused = announcements::post_unless_gone(&mut snapshot, &acme).await;

    assert!(refused.is_err(), "{refused:?}");
}

#[tokio::test]
async fn obs_07_an_event_stores_the_trace_context_that_published_it() {
    let api = fixtures::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    // The registration's session published its start (AUTH-046).
    api.database().owner_executes("DELETE FROM events").await;

    let response = send_keyed(&api, &ada, &json!({ "value": "a" }), None).await;

    // `server-timing: trace;desc=00-<trace ID>-<span ID>-<flags>` (API-16).
    let trace_id = response.header("server-timing").split('-').nth(1).unwrap();
    let traceparents = api
        .database()
        .owner_reads("SELECT traceparent FROM events")
        .await;
    assert_eq!(traceparents.len(), 1);
    assert_eq!(traceparents[0].split('-').nth(1), Some(trace_id));
}
