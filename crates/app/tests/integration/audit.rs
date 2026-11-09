//! The audit writer (the audit spec).

use crate::support::{Api, Options, users};

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
    let options = Options::default().set("TRUSTED_PROXIES", "198.51.100.0/24");
    let api = Api::with(options).await;

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
             FROM audit_entries entry JOIN audit_entry_clients client ON client.entry_id = entry.id",
        )
        .await;
    assert_eq!(
        entry,
        [format!("203.0.113.9 Desktop/1.2 {trace_id} {{}} true")]
    );
}

#[tokio::test]
async fn audit_005_the_app_role_can_t_change_or_delete_an_entry() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let db = api.database();

    for statement in [
        "UPDATE audit_entries SET action = 'forged'",
        "UPDATE audit_entries SET actor_user_id = NULL",
        "DELETE FROM audit_entries",
        "UPDATE audit_entry_clients SET client_ip = NULL",
        "DELETE FROM audit_entry_clients",
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
    let workspace = crate::support::workspaces::arrange(
        db,
        "Acme",
        &[(&bob.user_id, "owner"), (&ada.user_id, "admin")],
    )
    .await;
    // Ada acts in the workspace, so an entry of its log names her as its actor, with her client;
    // her leaving adds one that names her as its target.
    let renamed = api
        .patch(
            &format!("/v1/workspaces/{workspace}"),
            &serde_json::json!({ "name": "Acme Inc" }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(renamed.status, 200);
    let entries_before = db
        .owner_reads("SELECT count(*)::text FROM audit_entries WHERE workspace_id IS NOT NULL")
        .await;

    let deleted = api
        .request(http::Method::DELETE, "/v1/users/me")
        .bearer(&ada.access)
        .send()
        .await;

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
