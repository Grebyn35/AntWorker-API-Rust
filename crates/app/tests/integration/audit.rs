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
            "SELECT host(client_ip) || ' ' || user_agent || ' ' || trace_id || ' ' || \
             details::text FROM audit_entries",
        )
        .await;
    assert_eq!(entry, [format!("203.0.113.9 Desktop/1.2 {trace_id} {{}}")]);
}

#[tokio::test]
async fn audit_005_the_app_role_can_t_change_or_delete_an_entry() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let db = api.database();

    for statement in [
        "UPDATE audit_entries SET action = 'forged'",
        "DELETE FROM audit_entries",
    ] {
        let refused = db.try_execute(&db.app_url(), statement).await;

        assert!(
            refused.is_err_and(|error| error.contains("permission denied")),
            "{statement}"
        );
    }
}
