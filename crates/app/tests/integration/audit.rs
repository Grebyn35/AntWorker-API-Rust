//! The audit writer (the audit spec). The action catalog is in `catalog`, reading the logs in
//! `reading`, and retention in `retention`.

mod catalog;
mod reading;
mod retention;

use platform::telemetry::testing;

use crate::support::{Api, users, workspaces};

#[tokio::test]
async fn audit_001_an_entry_commits_or_rolls_back_with_its_change() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;

    let refused = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;

    assert_eq!(refused.status, 409);
    let actions = api
        .database()
        .owner_reads("SELECT action FROM audit_entries")
        .await;
    assert_eq!(actions, ["user.registered"]);
}

#[tokio::test]
async fn audit_001_an_entry_rolls_back_with_a_unit_of_work_that_fails_after_it_and_commits_with_one_that_succeeds()
 {
    let api = Api::new().await;
    let db = api.database();
    // A registration records `user.registered`, then issues the device token (the auth spec's
    // "Registration"), whose insert `app` may not make here, so the unit of work fails after
    // the entry.
    db.owner_executes("REVOKE INSERT ON device_tokens FROM app")
        .await;

    let failed = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    db.owner_executes("GRANT INSERT ON device_tokens TO app")
        .await;
    let registered = api
        .post("/v1/users", &users::registration("grace@example.com"))
        .send()
        .await;

    assert_eq!(failed.status, 500);
    assert!(
        testing::logged(failed.trace_id())
            .iter()
            .any(|line| line["exception.message"]
                .as_str()
                .is_some_and(|message| message.contains("device_tokens"))),
        "the insert of the device token failed"
    );
    assert_eq!(registered.status, 201, "{}", registered.json());
    let grace = registered.json()["user_id"].as_str().unwrap().to_owned();
    assert_eq!(
        db.owner_reads("SELECT action || ' ' || user_id FROM audit_entries")
            .await,
        [format!("user.registered {grace}")],
        "no entry of the failed unit of work stayed"
    );
    assert_eq!(
        db.owner_reads("SELECT email FROM users").await,
        ["grace@example.com"],
        "nor did the change it recorded"
    );
}

#[tokio::test]
async fn audit_002_an_entry_s_actor_and_log_come_from_the_proof() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    users::sign_in(&api, "ada@example.com", "wrong-password-123").await;

    let entries = api
        .database()
        .owner_reads(
            "SELECT action || ' ' || scope || ' ' || user_id || ' ' || \
             coalesce(actor_user_id::text, 'none') || ' ' || coalesce(workspace_id::text, 'none') \
             FROM audit_entries ORDER BY created_at, id",
        )
        .await;
    assert_eq!(
        entries,
        [
            format!("user.registered user {0} {0} none", ada.user_id),
            format!("sign_in.failed user {} none none", ada.user_id),
        ]
    );
}

#[tokio::test]
async fn audit_003_an_entry_records_the_client_its_trace_and_only_ids() {
    let api = reading::behind_a_proxy().await;

    let response = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .header("user-agent", "Desktop/1.2")
        .header("x-forwarded-for", "203.0.113.9")
        .send()
        .await;

    let trace_id = response
        .header("server-timing")
        .split('-')
        .nth(1)
        .unwrap()
        .to_owned();
    let entry = api
        .database()
        .owner_reads(
            "SELECT host(client.client_ip) || ' ' || client.user_agent || ' ' || entry.trace_id \
             || ' ' || entry.details::text || ' ' || (client.user_id = entry.user_id)::text \
             FROM audit_entries entry JOIN audit_entry_clients client ON client.audit_entry_id = entry.id",
        )
        .await;
    assert_eq!(
        entry,
        [format!("203.0.113.9 Desktop/1.2 {trace_id} {{}} true")]
    );
}

#[tokio::test]
async fn audit_005_the_app_role_changes_no_entry_and_deletes_none_from_the_tables() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let db = api.database();

    for statement in [
        "UPDATE audit_entries SET action = 'forged'",
        "UPDATE audit_entries SET actor_user_id = NULL",
        "DELETE FROM audit_entries",
        "UPDATE audit_entry_clients SET client_ip = NULL",
        "DELETE FROM audit_entry_clients",
        // The view through which retention deletes old entries changes none, and adds none.
        "UPDATE deletable_audit_entries SET created_at = now()",
        "INSERT INTO deletable_audit_entries (created_at) VALUES (now())",
    ] {
        let refused = db.try_execute(&db.app_url(), statement).await;

        assert!(
            refused.is_err_and(|error| error.contains("permission denied")),
            "{statement}"
        );
    }
}

#[tokio::test]
async fn audit_006_deleting_a_user_deletes_their_log_and_anonymizes_the_entries_that_name_them() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&bob.user_id, "owner"), (&ada.user_id, "admin")],
    )
    .await;
    // Ada acts in the workspace, so an entry of its log names her as its actor, with her client;
    // her leaving adds one that names her as its target.
    let renamed = workspaces::rename(&api, &ada, &workspace, "Acme Inc", &[]).await;
    assert_eq!(renamed.status, 200);
    let entries_before = db
        .owner_reads("SELECT count(*)::text FROM audit_entries WHERE workspace_id IS NOT NULL")
        .await;

    let deleted = users::delete(&api, &ada).await;

    assert_eq!(deleted.status, 204);
    let ada_id = &ada.user_id;
    let read = |sql: String| async move { db.owner_reads(&sql).await.remove(0) };
    assert_eq!(
        read(format!(
            "SELECT (SELECT count(*) FROM audit_entries WHERE user_id = '{ada_id}') || ' ' \
             || (SELECT count(*) FROM audit_entries entry WHERE entry::text LIKE '%{ada_id}%') \
             || ' ' || (SELECT count(*) FROM audit_entry_clients WHERE user_id = '{ada_id}')"
        ))
        .await,
        "0 0 0"
    );
    // The workspace's log keeps every entry, and the one that Ada's leaving adds.
    let entries_after = db
        .owner_reads("SELECT count(*)::text FROM audit_entries WHERE workspace_id IS NOT NULL")
        .await;
    let before: u32 = entries_before[0].parse().unwrap();
    assert_eq!(entries_after, [(before + 1).to_string()]);
    assert_eq!(
        read(
            "SELECT coalesce(actor_user_id::text, '-') || ' ' \
             || coalesce(target_user_id::text, '-') FROM audit_entries \
             WHERE action = 'workspace.renamed'"
                .to_owned()
        )
        .await,
        "- -"
    );
    // Bob's own log, and the clients of his entries, stay as they were.
    assert_eq!(
        read(format!(
            "SELECT count(*)::text FROM audit_entry_clients WHERE user_id = '{}'",
            bob.user_id
        ))
        .await,
        "1"
    );
}
