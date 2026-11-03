//! Builders of jobs, written as a unit of work enqueues them (TEST-10).

use serde_json::Value;

use super::TestDatabase;

/// Enqueues a job of `kind` with `payload`, as a unit of work that ran in the trace that
/// `traceparent` names enqueues it, through the `owner` connection, for a test whose API runs in
/// a process of its own, where no span of the test's encloses the enqueueing.
pub async fn enqueued(db: &TestDatabase, kind: &str, payload: &Value, traceparent: &str) {
    let payload = payload.to_string().replace('\'', "''");
    db.owner_executes(&format!(
        "INSERT INTO jobs (kind, payload, traceparent) \
         VALUES ('{kind}', '{payload}', '{traceparent}')"
    ))
    .await;
}
