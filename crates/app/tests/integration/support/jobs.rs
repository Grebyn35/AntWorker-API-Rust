//! Builders of jobs, written as a unit of work enqueues them (TEST-09).

use std::time::Duration;

use serde_json::Value;

use super::TestDatabase;

/// The interval of a periodic job that runs every hour.
pub const HOUR: Duration = Duration::from_secs(3_600);

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

// part: files
/// Gives the periodic jobs `names` the schedule of a run due a day from now, as one that ran
/// does, through the `owner` connection, so a `serve` that starts on `db` runs none of them
/// while the test runs (PLATFORM-015).
pub async fn not_due(db: &TestDatabase, names: &[&str]) {
    for name in names {
        db.owner_executes(&format!(
            "INSERT INTO job_schedules (name, next_run_at) VALUES ('{name}', now() + interval \
             '1 day') ON CONFLICT ON CONSTRAINT job_schedules_name_key DO UPDATE SET next_run_at \
             = EXCLUDED.next_run_at"
        ))
        .await;
    }
}
// end part: files
