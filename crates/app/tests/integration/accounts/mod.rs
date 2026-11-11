//! The user record (the accounts spec).

mod deletion;
pub(crate) mod exports;

use serde_json::json;

use crate::support::{
    Api,
    api::Response,
    events::{self, Event},
    execute_on, queries_wait_on_locks, queries_waiting_on_locks, streams,
    users::{self, PASSWORD, SignedIn},
    workspaces,
};

#[tokio::test]
async fn accounts_001_the_caller_reads_their_own_record_and_no_other_user_s() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::register(&api, "grace@example.com").await;

    let response = api.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(response.status, 200);
    // Proving the address, which the builder records, changed the record once (AUTH-031).
    assert_eq!(response.header("etag"), "\"2\"");
    let record = response.json();
    // part: files
    let record = {
        let mut record = record;
        let avatar = record.as_object_mut().unwrap().remove("avatar_url");
        assert_eq!(avatar, Some(json!(null)));
        record
    };
    // end part: files
    assert_eq!(
        record,
        json!({
            "id": ada.user_id,
            "email": "ada@example.com",
            "name": "Ada Lovelace",
            "email_verified": true,
        })
    );
}

#[tokio::test]
async fn accounts_002_an_email_address_is_stored_trimmed_and_lowercased() {
    let api = Api::new().await;
    let body = users::registration("  Ada@Example.COM ");

    let response = api.post_raw("/v1/users", &body).send().await;

    assert_eq!(response.status, 201);

    let stored = api.database().owner_reads("SELECT email FROM users").await;
    assert_eq!(stored, ["ada@example.com"]);
}

#[tokio::test]
async fn accounts_002_an_invalid_email_address_is_refused() {
    let api = Api::new().await;

    let response = api
        .post_raw("/v1/users", &users::registration("not-an-address"))
        .send()
        .await;

    assert_eq!(response.status, 400);
    let body = response.json();
    assert_eq!(body["code"], "request.invalid_fields");
    assert_eq!(
        body["errors"],
        json!([{ "field": "/email", "code": "invalid_email", "message": "This isn't an email address." }])
    );
}

#[tokio::test]
async fn accounts_003_a_display_name_is_stored_trimmed_collapsed_and_normalized() {
    let api = Api::new().await;
    let body = json!({
        "email": "ada@example.com",
        "password": PASSWORD,
        "name": "  Rene\u{301}e \t  Lovelace ",
    });

    let response = api.post("/v1/users", &body).send().await;

    assert_eq!(response.status, 201);
    let ada = SignedIn::from(&response);
    users::verified(&api, &ada.user_id).await;
    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(me.json()["name"], "Ren\u{e9}e Lovelace");
}

#[tokio::test]
async fn accounts_004_no_two_users_have_the_same_email() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;

    let response = api
        .post("/v1/users", &users::registration("ADA@example.com"))
        .send()
        .await;

    assert_eq!(response.status, 409);
    let count = api
        .database()
        .owner_reads("SELECT count(*)::text FROM users")
        .await;
    assert_eq!(count, ["1"]);
}

/// Renames `user` to `name` with the merge patch of ACCOUNTS-007, with `if_match` if given.
async fn rename(api: &Api, user: &SignedIn, name: &str, if_match: Option<&str>) -> Response {
    let mut call = api
        .patch("/v1/users/me", &json!({ "name": name }))
        .bearer(&user.access);
    if let Some(etag) = if_match {
        call = call.header("if-match", etag);
    }
    call.send().await
}

/// The name of `user` that the database holds.
async fn stored_name(api: &Api, user: &SignedIn) -> String {
    api.database()
        .owner_reads(&format!(
            "SELECT name FROM users WHERE id = '{}'",
            user.user_id
        ))
        .await
        .remove(0)
}

#[tokio::test]
async fn accounts_007_renaming_oneself_stores_the_name_records_it_and_publishes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(db, "Acme", &[(&grace.user_id, "owner")]).await;
    workspaces::join(db, &acme, &ada.user_id, "member").await;
    let labs = workspaces::arrange(db, "Labs", &[(&ada.user_id, "owner")]).await;
    workspaces::arrange(db, "Elsewhere", &[(&grace.user_id, "owner")]).await;
    // The registrations' sessions published their starts (AUTH-046).
    db.owner_executes("DELETE FROM events").await;

    let response = rename(&api, &ada, "  Ada \t King ", None).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.json()["id"], ada.user_id);
    assert_eq!(response.json()["name"], "Ada King");
    assert_eq!(response.header("etag"), "\"3\"");
    assert_eq!(stored_name(&api, &ada).await, "Ada King");
    let entries = db
        .owner_reads(
            "SELECT scope || ' ' || user_id || ' ' || actor_user_id || ' ' || resource_type \
             || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = 'user.renamed'",
        )
        .await;
    let ada_id = &ada.user_id;
    assert_eq!(
        entries,
        [format!("user {ada_id} {ada_id} user {ada_id} {{}}")]
    );
    let events = db
        .owner_reads(
            "SELECT name || ' ' || scope || ' ' || coalesce(user_id::text, '-') || ' ' \
             || coalesce(workspace_id::text, '-') || ' ' || coalesce(permission, '-') || ' ' \
             || payload::text FROM events ORDER BY name, workspace_id",
        )
        .await;
    let mut expected = vec![
        format!(
            "member.updated workspace - {acme} members.read {{\"user_id\": \"{ada_id}\", \"workspace_id\": \"{acme}\"}}"
        ),
        format!(
            "member.updated workspace - {labs} members.read {{\"user_id\": \"{ada_id}\", \"workspace_id\": \"{labs}\"}}"
        ),
        format!("user.updated user {ada_id} - - {{\"user_id\": \"{ada_id}\"}}"),
    ];
    expected[..2].sort();
    assert_eq!(events, expected);
}

#[tokio::test]
async fn accounts_007_a_rename_with_a_stale_etag_answers_412_and_changes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let etag = api
        .get("/v1/users/me")
        .bearer(&ada.access)
        .send()
        .await
        .header("etag")
        .to_owned();

    let first = rename(&api, &ada, "Ada King", Some(&etag)).await;
    let stale = rename(&api, &ada, "Ada Byron", Some(&etag)).await;
    let current = rename(&api, &ada, "Ada Lovelace", Some(first.header("etag"))).await;

    assert_eq!(first.status, 200);
    assert_eq!(stale.status, 412);
    assert_eq!(stale.json()["code"], "request.precondition_failed");
    assert_eq!(current.status, 200);
    assert_eq!(current.header("etag"), "\"4\"");
    let recorded = api
        .database()
        .owner_reads(
            "SELECT (SELECT count(*) FROM audit_entries WHERE action = 'user.renamed') \
             || ' ' || (SELECT count(*) FROM events WHERE name = 'user.updated')",
        )
        .await;
    assert_eq!(recorded, ["2 2"]);
}

#[tokio::test]
async fn accounts_003_a_new_name_is_refused_unless_it_holds_a_character() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let blank = rename(&api, &ada, " \t ", None).await;
    // A name longer than the document allows, or a field the patch doesn't take, is malformed
    // input, sent raw.
    let raw = |body: serde_json::Value| {
        api.request(http::Method::PATCH, "/v1/users/me")
            .header("content-type", "application/merge-patch+json")
            .body(body.to_string())
            .bearer(&ada.access)
    };
    let long = raw(json!({ "name": "a".repeat(101) })).send().await;
    let email = raw(json!({ "name": "Ada", "email": "eve@example.com" }))
        .send()
        .await;

    for (refused, code) in [(&blank, "required"), (&long, "too_long")] {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "request.invalid_fields");
        assert_eq!(refused.json()["errors"][0]["field"], "/name");
        assert_eq!(refused.json()["errors"][0]["code"], code);
    }
    assert_eq!(email.status, 400);
    assert_eq!(email.json()["code"], "request.invalid_body");
    assert_eq!(stored_name(&api, &ada).await, "Ada Lovelace");
    let stored = api
        .database()
        .owner_reads(&format!(
            "SELECT email || ' ' || version FROM users WHERE id = '{}'",
            ada.user_id
        ))
        .await;
    assert_eq!(stored, ["ada@example.com 2"]);
}

#[tokio::test]
async fn sec_19_member_updated_reaches_the_members_and_no_replay_from_before_joining() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(
        db,
        "Acme",
        &[(&grace.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    // An event to Alan alone gives his stream one to resume after.
    db.owner_executes(&events::statement(
        &[Event::to_user(&alan.user_id, "note.made", "{}")],
        true,
    ))
    .await;
    let before = streams::opened(&api, &alan).await.last_event_id;
    // Every role holds members.read, which member.updated requires: no member lacks it.
    let mut own = streams::opened(&api, &ada).await;
    let mut member = streams::opened(&api, &grace).await;

    let renamed = rename(&api, &ada, "Ada King", None).await;
    workspaces::join(db, &acme, &alan.user_id, "member").await;
    let mut joined = streams::connect(&api, &alan.access, before.as_deref()).await;
    joined.opened().await;
    let again = rename(&api, &ada, "Ada Byron", None).await;
    let after_joining = joined.event().await;

    assert_eq!(renamed.status, 200);
    assert_eq!(again.status, 200);
    let updated = json!({ "workspace_id": acme, "user_id": ada.user_id });
    // Ada's own streams receive her user event, and, as a member, the workspace's.
    for _ in 0..2 {
        let user = own.event().await;
        assert_eq!(user.event.as_deref(), Some("user.updated"));
        assert_eq!(user.payload(), &json!({ "user_id": ada.user_id }));
        let workspace = own.event().await;
        assert_eq!(workspace.event.as_deref(), Some("member.updated"));
        assert_eq!(workspace.payload(), &updated);
    }
    let mut seen = Vec::new();
    for _ in 0..2 {
        let event = member.event().await;
        assert_eq!(event.event.as_deref(), Some("member.updated"));
        assert_eq!(event.payload(), &updated);
        seen.push(event.id);
    }
    // The rename from before Alan joined isn't replayed to him: the first he receives is the one
    // made after.
    assert_eq!(after_joining.event.as_deref(), Some("member.updated"));
    assert_eq!(after_joining.id, seen[1]);
}

#[tokio::test]
async fn accounts_019_a_rename_that_waited_for_the_account_s_deletion_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    // A deletion of the account that holds the user's row, and their sessions with it.
    let mut deletion = db
        .holding(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    let renamed = api.spawn(
        http::Method::PATCH,
        "/v1/users/me",
        &ada.access,
        Some(json!({ "name": "Ada King" })),
    );
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, body) = renamed.await.unwrap();

    // Its turn came once the user was gone, and their session with them.
    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
}

#[tokio::test]
async fn accounts_019_a_read_that_finds_the_account_gone_answers_401() {
    // With email off, authenticating reads the session alone, so the read of the record is the
    // request's first statement that reads `users`.
    let api = Api::without_email().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let mut deletion = db
        .holding("LOCK TABLE users IN ACCESS EXCLUSIVE MODE")
        .await;

    let read = api.spawn(http::Method::GET, "/v1/users/me", &ada.access, None);
    queries_wait_on_locks(db, 1).await;
    let waiting = queries_waiting_on_locks(db).await;
    assert!(waiting[0].contains("FROM users WHERE id"), "{waiting:?}");
    execute_on(
        &mut deletion,
        &format!("DELETE FROM users WHERE id = '{}'", ada.user_id),
    )
    .await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, body) = read.await.unwrap();

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
}
