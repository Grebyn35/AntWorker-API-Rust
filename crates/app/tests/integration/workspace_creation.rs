//! Users creating workspaces (the workspaces spec's "Users creating workspaces").

use serde_json::json;

use crate::support::{
    Api,
    users::{self, SignedIn},
};

/// Creates a workspace named `name` as `user`, with `key` as its idempotency key if given.
async fn create(
    api: &Api,
    user: &SignedIn,
    name: &str,
    key: Option<&str>,
) -> crate::support::api::Response {
    let mut call = api
        .post("/v1/workspaces", &json!({ "name": name }))
        .bearer(&user.access);
    if let Some(key) = key {
        call = call.header("idempotency-key", key);
    }
    call.send().await
}

#[tokio::test]
async fn workspaces_001_creating_a_workspace_makes_the_caller_its_owner() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // The registration's session published its start (AUTH-046).
    api.database().owner_executes("DELETE FROM events").await;

    let response = create(&api, &ada, "Acme", None).await;

    assert_eq!(response.status, 201);
    let body = response.json();
    let id = body["id"].as_str().unwrap();
    assert_eq!(response.header("location"), format!("/v1/workspaces/{id}"));
    assert_eq!(response.header("etag"), "\"1\"");
    assert_eq!(body["name"], "Acme");
    assert_eq!(body["role"], "owner");
    assert!(body["created_at"].as_str().unwrap().ends_with('Z'));
    let db = api.database();
    let members = db
        .owner_reads("SELECT user_id || ' ' || role FROM workspace_memberships")
        .await;
    assert_eq!(members, [format!("{} owner", ada.user_id)]);
    let entries = db
        .owner_reads(
            "SELECT action || ' ' || scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
             || resource_type || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = 'workspace.created'",
        )
        .await;
    assert_eq!(
        entries,
        [format!(
            "workspace.created workspace {id} {} workspace {id} {{}}",
            ada.user_id
        )]
    );
    let events = db
        .owner_reads(
            "SELECT name || ' ' || scope || ' ' || user_id || ' ' || (payload->>'workspace_id')              || ' ' || (payload->>'session_id') FROM events",
        )
        .await;
    assert_eq!(
        events,
        [format!(
            "membership.created user {} {id} {}",
            ada.user_id, ada.session_id
        )]
    );
}

#[tokio::test]
async fn workspaces_001_a_name_is_stored_normalized_and_must_hold_a_character() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let created = create(&api, &ada, "  Acme \t  Labs ", None).await;
    let refused = create(&api, &ada, " \t ", None).await;

    assert_eq!(created.json()["name"], "Acme Labs");
    assert_eq!(refused.status, 400);
    assert_eq!(refused.json()["code"], "request.invalid_fields");
    assert_eq!(refused.json()["errors"][0]["field"], "/name");
    assert_eq!(refused.json()["errors"][0]["code"], "required");
    let names = api
        .database()
        .owner_reads("SELECT name FROM workspaces")
        .await;
    assert_eq!(names, ["Acme Labs"]);
}

#[tokio::test]
async fn workspaces_001_a_retry_with_the_key_creates_one_workspace() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let first = create(&api, &ada, "Acme", Some("\"k-1\"")).await;
    let retry = create(&api, &ada, "Acme", Some("\"k-1\"")).await;

    assert_eq!(first.status, 201);
    assert_eq!(retry.status, 201);
    assert_eq!(retry.json(), first.json());
    assert_eq!(retry.header("location"), first.header("location"));
    assert_eq!(retry.header("etag"), first.header("etag"));
    let count = api
        .database()
        .owner_reads("SELECT count(*)::text FROM workspaces")
        .await;
    assert_eq!(count, ["1"]);
}

#[tokio::test]
async fn workspaces_001_a_created_workspace_s_location_reads_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let created = create(&api, &ada, "Acme", None).await;

    let read = api
        .get(created.header("location"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(read.status, 200);
    assert_eq!(read.json(), created.json());
    assert_eq!(read.header("etag"), created.header("etag"));
}
