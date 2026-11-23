//! Retention (the audit spec's "Retention"): the view through which `app` deletes entries, and
//! the hourly job that deletes those older than the retention.

use crate::support::{Api, Options, audit, execute_on, users, workspaces};

/// The periodic job of retention.
const JOB: &str = "audit.expire_entries";

/// Inserts, as the owner may, `count` entries in the own log of `user_id`, each recorded `age`
/// ago, with the action `user.renamed`.
async fn arrange(api: &Api, user_id: &str, age: &str, count: u32) {
    api.database()
        .owner_executes(&format!(
            "INSERT INTO audit_entries (scope, user_id, action, resource_type, created_at) \
             SELECT 'user', '{user_id}', 'user.renamed', 'user', now() - interval '{age}' \
             FROM generate_series(1, {count})"
        ))
        .await;
}

/// The whole days since each entry of `action` was recorded, the oldest first.
async fn ages(api: &Api, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT extract(day FROM now() - created_at)::text FROM audit_entries \
             WHERE action = '{action}' ORDER BY created_at, id"
        ))
        .await
}

#[tokio::test]
async fn audit_005_app_deletes_through_the_view_only_the_entries_recorded_more_than_90_days_ago() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    for age in ["400 days", "90 days 1 hour", "89 days 23 hours"] {
        arrange(&api, &ada.user_id, age, 1).await;
    }
    // The oldest has a client, which goes with it.
    let oldest = db
        .owner_reads(
            "SELECT id::text FROM audit_entries WHERE action = 'user.renamed' \
             ORDER BY created_at LIMIT 1",
        )
        .await
        .remove(0);
    audit::with_client(
        db,
        &oldest,
        &audit::Client {
            user: &ada.user_id,
            ip: "203.0.113.9",
            agent: "Desktop/1.2",
        },
    )
    .await;
    let clients_before = db
        .owner_reads("SELECT count(*)::text FROM audit_entry_clients")
        .await;

    // Every entry that the view shows, whatever the statement asks.
    db.try_execute(&db.app_url(), "DELETE FROM deletable_audit_entries")
        .await
        .unwrap();

    assert_eq!(ages(&api, "user.renamed").await, ["89"]);
    let clients_after = db
        .owner_reads("SELECT count(*)::text FROM audit_entry_clients")
        .await;
    let before: u32 = clients_before[0].parse().unwrap();
    assert_eq!(clients_after, [(before - 1).to_string()]);
    // The entries that a request recorded just now stay too.
    assert_eq!(ages(&api, "user.registered").await, ["0"]);
}

#[tokio::test]
async fn audit_005_the_view_s_floor_holds_whatever_time_zone_the_session_sets() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    for age in ["77 days", "85 days", "89 days 23 hours"] {
        arrange(&api, &ada.user_id, age, 1).await;
    }
    // A zone that `app` may set, whose offset changes by 334 hours once between 90 days ago and
    // now: its daylight time starts 45 days ago and ends 180 days later, one way round and the
    // other. Subtracting 90 days in it, rather than 2,160 hours, moves the floor by as much.
    let start: u32 = db
        .owner_reads(
            "SELECT (mod(extract(doy FROM now() - interval '45 days')::int - 1, 365) + 1)::text",
        )
        .await[0]
        .parse()
        .unwrap();
    let end = (start - 1 + 180) % 365 + 1;
    for zone in [
        format!("AAA+167BBB-167,J{start},J{end}"),
        format!("AAA-167BBB+167,J{start},J{end}"),
    ] {
        db.try_execute(
            &db.app_url(),
            &format!("SET TimeZone = '{zone}'; DELETE FROM deletable_audit_entries"),
        )
        .await
        .unwrap();
    }

    assert_eq!(ages(&api, "user.renamed").await, ["89", "85", "77"]);
}

#[tokio::test]
async fn audit_011_the_job_runs_every_hour() {
    let api = Api::new().await;

    let interval = api.interval("audit.expire_entries");

    assert_eq!(interval, crate::support::jobs::HOUR);
}

#[tokio::test]
async fn audit_011_the_hourly_job_deletes_each_log_s_entries_older_than_the_retention_with_their_clients()
 {
    let api = Api::with(Options::default().set("AUDIT_RETENTION_DAYS", 100)).await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    for name in ["Acme Inc", "Acme Ltd"] {
        let renamed = api
            .patch(
                &format!("/v1/workspaces/{workspace}"),
                &serde_json::json!({ "name": name }),
            )
            .bearer(&ada.access)
            .send()
            .await;
        assert_eq!(renamed.status, 200);
    }
    // Ada's own log, and the workspace's first rename, are older than the retention; the
    // workspace's second rename isn't. Each was recorded in a request, with its client.
    db.owner_executes(
        "UPDATE audit_entries SET created_at = now() - interval '101 days' WHERE scope = 'user'",
    )
    .await;
    db.owner_executes(
        "UPDATE audit_entries SET created_at = now() - interval '101 days' WHERE id = ( \
         SELECT id FROM audit_entries WHERE scope = 'workspace' ORDER BY created_at, id LIMIT 1)",
    )
    .await;
    db.owner_executes(
        "UPDATE audit_entries SET created_at = now() - interval '99 days' \
         WHERE scope = 'workspace' AND created_at > now() - interval '1 day'",
    )
    .await;
    let kept = db
        .owner_reads(
            "SELECT id::text FROM audit_entries WHERE created_at > now() - interval '100 days'",
        )
        .await;
    assert_eq!(kept.len(), 1);
    let user_entries = db
        .owner_reads("SELECT count(*)::text FROM audit_entries WHERE scope = 'user'")
        .await;
    assert_ne!(user_entries, ["0"]);

    api.run_periodic(JOB).await;

    let left = db
        .owner_reads("SELECT id::text FROM audit_entries ORDER BY id")
        .await;
    assert_eq!(left, kept);
    let clients = db
        .owner_reads("SELECT audit_entry_id::text FROM audit_entry_clients")
        .await;
    assert_eq!(clients, kept);
    // Scheduled maintenance records no entry (DATA-22), and a second run deletes nothing more.
    api.run_periodic(JOB).await;
    assert_eq!(
        db.owner_reads("SELECT id::text FROM audit_entries ORDER BY id")
            .await,
        kept
    );
}

#[tokio::test]
async fn audit_011_by_default_a_log_keeps_an_entry_365_days_and_a_run_deletes_every_batch() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // More than two batches of 1,000 past the retention, and one entry short of it.
    arrange(&api, &ada.user_id, "366 days", 2_500).await;
    arrange(&api, &ada.user_id, "364 days", 1).await;

    api.run_periodic(JOB).await;

    assert_eq!(ages(&api, "user.renamed").await, ["364"]);
}

#[tokio::test]
async fn audit_011_a_log_keeps_an_entry_90_to_3650_days() {
    for days in [89, 3_651] {
        let refused =
            crate::support::api::refusal(&Options::default().set("AUDIT_RETENTION_DAYS", days));

        assert_eq!(
            refused,
            "AUDIT_RETENTION_DAYS must be a whole number from 90 to 3650"
        );
    }
}

#[tokio::test]
async fn audit_011_a_run_that_meets_an_entry_that_another_transaction_holds_ends_and_a_later_run_deletes_it()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    arrange(&api, &ada.user_id, "400 days", 3).await;
    // A transaction that sets an entry's actor to null, as an account's deletion does, and
    // hasn't committed.
    let mut held = api
        .database()
        .holding(
            "UPDATE audit_entries SET actor_user_id = NULL WHERE id = ( \
             SELECT id FROM audit_entries WHERE action = 'user.renamed' ORDER BY id LIMIT 1)",
        )
        .await;

    // The run waits for no lock, so it ends while the entry stays held.
    api.run_periodic(JOB).await;
    let during = ages(&api, "user.renamed").await;
    execute_on(&mut held, "ROLLBACK").await;
    api.run_periodic(JOB).await;

    assert_eq!(during, ["400", "400", "400"]);
    assert!(ages(&api, "user.renamed").await.is_empty());
}
