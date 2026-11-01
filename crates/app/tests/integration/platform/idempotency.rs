//! Idempotency keys (the platform spec's "Idempotency"), and publishing events in a unit of work
//! (the platform spec's "Events and the real-time hub"), through the tests' keyed route, so they
//! hold whichever optional parts a fork keeps.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::support::{
    Api, Options, execute_on, fixtures,
    users::{self, SignedIn},
};

/// The tests' API, with their keyed route.
async fn api() -> Api {
    Api::with(Options::routes(fixtures::routes(Arc::default()))).await
}

/// Sends `body` to the keyed route as `user`, with `key` as its idempotency key if given.
async fn keyed(
    api: &Api,
    user: &SignedIn,
    body: &Value,
    key: Option<&str>,
) -> crate::support::api::Response {
    let mut call = api.post("/v1/keyed", body).bearer(&user.access);
    if let Some(key) = key {
        call = call.header("idempotency-key", key);
    }
    call.send().await
}

#[tokio::test]
async fn platform_018_a_retry_with_the_key_answers_the_first_result_and_runs_nothing() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let body = json!({ "value": "a" });

    let first = keyed(&api, &ada, &body, Some("\"k-1\"")).await;
    let retry = keyed(&api, &ada, &body, Some("\"k-1\"")).await;
    let unkeyed = keyed(&api, &ada, &body, None).await;

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
async fn platform_018_the_key_with_another_request_answers_422() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    keyed(&api, &ada, &json!({ "value": "a" }), Some("\"k-1\"")).await;

    let reused = keyed(&api, &ada, &json!({ "value": "b" }), Some("\"k-1\"")).await;

    assert_eq!(reused.status, 422);
    assert_eq!(reused.json()["code"], "request.idempotency_key_reused");
}

#[tokio::test]
async fn platform_018_a_key_whose_first_request_hasn_t_committed_answers_409_then_runs() {
    let api = api().await;
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

    let waiting = keyed(&api, &ada, &json!({ "value": "a" }), Some("\"k-1\"")).await;
    execute_on(&mut held, "ROLLBACK").await;
    let retry = keyed(&api, &ada, &json!({ "value": "a" }), Some("\"k-1\"")).await;

    assert_eq!(waiting.status, 409);
    assert_eq!(waiting.json()["code"], "request.idempotency_key_in_flight");
    assert_eq!(retry.status, 200);
    assert_eq!(retry.json()["run"], 1);
}

#[tokio::test]
async fn platform_018_an_operation_that_rolled_back_leaves_no_record_so_its_retry_runs() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;

    let failed = keyed(
        &api,
        &ada,
        &json!({ "value": "a", "fail": true }),
        Some("\"k-1\""),
    )
    .await;
    let retry = keyed(
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
async fn platform_018_a_record_older_than_24_hours_counts_as_absent() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    keyed(&api, &ada, &json!({ "value": "a" }), Some("\"k-1\"")).await;
    api.database()
        .owner_executes("UPDATE idempotency_records SET created_at = now() - interval '25 hours'")
        .await;

    let again = keyed(&api, &ada, &json!({ "value": "b" }), Some("\"k-1\"")).await;

    assert_eq!(again.status, 200);
    assert_eq!(again.json(), json!({ "value": "b", "run": 2 }));
}

#[tokio::test]
async fn platform_058_a_malformed_key_answers_400_and_keys_are_per_user() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let body = json!({ "value": "a" });

    let unquoted = keyed(&api, &ada, &body, Some("k-1")).await;
    let too_long = keyed(&api, &ada, &body, Some(&format!("\"{}\"", "k".repeat(256)))).await;
    let ada_first = keyed(&api, &ada, &body, Some("\"k-1\"")).await;
    let grace_same_key = keyed(&api, &grace, &body, Some("\"k-1\"")).await;

    for refused in [&unquoted, &too_long] {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "request.invalid_idempotency_key");
    }
    assert_eq!(ada_first.json()["run"], 1);
    assert_eq!(grace_same_key.json()["run"], 2);
}

#[tokio::test]
async fn platform_015_a_published_event_exists_only_if_its_unit_of_work_commits() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;

    keyed(&api, &ada, &json!({ "value": "a", "fail": true }), None).await;
    let rolled_back = api.database().owner_reads("SELECT type FROM events").await;
    keyed(&api, &ada, &json!({ "value": "a" }), None).await;
    let committed = api
        .database()
        .owner_reads("SELECT type || ' ' || scope || ' ' || user_id FROM events")
        .await;

    assert!(rolled_back.is_empty(), "{rolled_back:?}");
    assert_eq!(committed, [format!("keyed.ran user {}", ada.user_id)]);
}

#[tokio::test]
async fn obs_07_an_event_stores_the_trace_context_that_published_it() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = keyed(&api, &ada, &json!({ "value": "a" }), None).await;

    // `server-timing: trace;desc=00-<trace ID>-<span ID>-<flags>` (API-16).
    let trace_id = response.header("server-timing").split('-').nth(1).unwrap();
    let traceparents = api
        .database()
        .owner_reads("SELECT traceparent FROM events")
        .await;
    assert_eq!(traceparents.len(), 1);
    assert_eq!(traceparents[0].split('-').nth(1), Some(trace_id));
}
