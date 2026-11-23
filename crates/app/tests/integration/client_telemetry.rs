//! Each user's consent to the telemetry of their clients: reading it, choosing it, and what
//! becomes of it with the account (the client_telemetry spec, CLIENT_TELEMETRY-001 to
//! CLIENT_TELEMETRY-003). The telemetry proxy's tests are in `proxy`.

mod proxy;

use http::Method;
use serde_json::{Value, json};

use crate::{
    accounts::exports::{built, document},
    support::{
        Api, execute_on, query_waits_on_a_lock,
        users::{self, SignedIn},
    },
};

/// The consent's route.
const CONSENT: &str = "/v1/users/me/telemetry-consent";

/// Reads `user`'s consent: the response's status, its body and its `ETag`.
async fn read(api: &Api, user: &SignedIn) -> (u16, Value, String) {
    let response = api.get(CONSENT).bearer(&user.access).send().await;
    let etag = response.header("etag").to_owned();
    (response.status.as_u16(), response.json(), etag)
}

/// Has `user` choose `consented`, with `if_match` as the request's `If-Match` when there is one.
async fn choose(
    api: &Api,
    user: &SignedIn,
    consented: bool,
    if_match: Option<&str>,
) -> crate::support::api::Response {
    let call = api
        .put(CONSENT, &json!({ "consented": consented }))
        .bearer(&user.access);
    match if_match {
        Some(etag) => call.header("if-match", etag),
        None => call,
    }
    .send()
    .await
}

/// The log, actor, resource and details of each entry that records a choice, the oldest first.
async fn entries(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT scope || ' ' || user_id || ' ' || actor_user_id || ' ' || resource_type \
             || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = 'telemetry_consent.updated' ORDER BY id",
        )
        .await
}

/// The scope, user and payload of each event that a choice published, the oldest first.
async fn events(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT scope || ' ' || user_id || ' ' || payload::text FROM events \
             WHERE name = 'telemetry_consent.updated' ORDER BY position",
        )
        .await
}

/// Each user's row of `telemetry_consents`, as its user, whether they consent, and its version.
async fn rows(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT user_id || ' ' || consented || ' ' || version FROM telemetry_consents \
             ORDER BY id",
        )
        .await
}

#[tokio::test]
async fn client_telemetry_001_a_user_who_never_chose_consents_to_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let (status, body, etag) = read(&api, &ada).await;

    assert_eq!(status, 200);
    assert_eq!(body, json!({ "consented": false }));
    assert_eq!(etag, "\"0\"");
    // Reading writes nothing.
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn client_telemetry_001_a_user_reads_their_own_consent_and_no_other() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    assert_eq!(choose(&api, &ada, true, None).await.status, 200);

    let (_, ada_reads, ada_etag) = read(&api, &ada).await;
    let (_, grace_reads, grace_etag) = read(&api, &grace).await;

    assert_eq!(ada_reads, json!({ "consented": true }));
    assert_eq!(ada_etag, "\"1\"");
    assert_eq!(grace_reads, json!({ "consented": false }));
    assert_eq!(grace_etag, "\"0\"");
}

#[tokio::test]
async fn client_telemetry_002_a_choice_stores_the_consent_records_it_and_publishes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let given = choose(&api, &ada, true, None).await;
    let after_giving = read(&api, &ada).await;
    let withdrawn = choose(&api, &ada, false, None).await;
    let after_withdrawing = read(&api, &ada).await;

    assert_eq!(given.status, 200);
    assert_eq!(given.json(), json!({ "consented": true }));
    assert_eq!(given.header("etag"), "\"1\"");
    assert_eq!(after_giving.1, json!({ "consented": true }));
    assert_eq!(after_giving.2, "\"1\"");
    assert_eq!(withdrawn.status, 200);
    assert_eq!(withdrawn.json(), json!({ "consented": false }));
    assert_eq!(withdrawn.header("etag"), "\"2\"");
    assert_eq!(after_withdrawing.1, json!({ "consented": false }));
    assert_eq!(after_withdrawing.2, "\"2\"");
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api).await,
        [
            format!("user {ada_id} {ada_id} user {ada_id} {{\"consented\": true}}"),
            format!("user {ada_id} {ada_id} user {ada_id} {{\"consented\": false}}"),
        ]
    );
    assert_eq!(
        events(&api).await,
        [
            format!("user {ada_id} {{\"consented\": true}}"),
            format!("user {ada_id} {{\"consented\": false}}"),
        ]
    );
}

#[tokio::test]
async fn client_telemetry_002_a_choice_of_the_stored_consent_writes_records_and_publishes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let first = choose(&api, &ada, true, None).await;
    let again = choose(&api, &ada, true, None).await;

    assert_eq!(first.header("etag"), "\"1\"");
    assert_eq!(again.status, 200);
    assert_eq!(again.header("etag"), "\"2\"");
    assert_eq!(entries(&api).await.len(), 2);
    assert_eq!(events(&api).await.len(), 2);
}

#[tokio::test]
async fn client_telemetry_002_a_choice_with_a_stale_etag_answers_412_and_changes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let (_, _, never) = read(&api, &ada).await;

    // The tag of a consent no one chose names it, until the first choice.
    let first = choose(&api, &ada, true, Some(&never)).await;
    let stale = choose(&api, &ada, false, Some(&never)).await;
    let current = choose(&api, &ada, false, Some(first.header("etag"))).await;
    // A tag that names a version the consent of a user who never chose doesn't hold.
    let unchosen = choose(&api, &grace, true, Some("\"1\"")).await;

    assert_eq!(first.status, 200);
    assert_eq!(first.header("etag"), "\"1\"");
    for refused in [&stale, &unchosen] {
        assert_eq!(refused.status, 412);
        assert_eq!(refused.json()["code"], "request.precondition_failed");
    }
    assert_eq!(current.status, 200);
    assert_eq!(current.header("etag"), "\"2\"");
    assert_eq!(rows(&api).await, [format!("{} false 2", ada.user_id)]);
    assert_eq!(entries(&api).await.len(), 2);
    assert_eq!(events(&api).await.len(), 2);
}

#[tokio::test]
async fn client_telemetry_002_a_body_that_isn_t_the_whole_consent_is_refused() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let raw = |body: Value| {
        api.request(Method::PUT, CONSENT)
            .header("content-type", "application/json")
            .body(body.to_string())
            .bearer(&ada.access)
    };

    let empty = raw(json!({})).send().await;
    let text = raw(json!({ "consented": "yes" })).send().await;
    let more = raw(json!({ "consented": true, "user_id": ada.user_id }))
        .send()
        .await;

    for refused in [&empty, &text, &more] {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "request.invalid_body");
    }
    assert!(rows(&api).await.is_empty());
    assert!(entries(&api).await.is_empty());
    assert!(events(&api).await.is_empty());
}

/// Has `user` choose to consent, in a task of its own: a request that the test holds at a lock.
fn spawn_choice(api: &Api, user: &SignedIn) -> tokio::task::JoinHandle<(u16, Value)> {
    let (api, access) = (api.clone(), user.access.clone());
    tokio::spawn(async move {
        let response = api
            .put(CONSENT, &json!({ "consented": true }))
            .bearer(&access)
            .send()
            .await;
        (response.status.as_u16(), response.json())
    })
}

#[tokio::test]
async fn client_telemetry_002_a_first_choice_that_waited_for_the_account_s_deletion_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    // A deletion of the account, which its unit of work holds.
    let mut deletion = db
        .holding(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    let chosen = spawn_choice(&api, &ada);
    // The insert of the consent's row waits for the user's, which its foreign key reads.
    query_waits_on_a_lock(db, "INSERT INTO telemetry_consents").await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, body) = chosen.await.unwrap();

    // Its turn came once the user was gone, as a request sent after the deletion is refused.
    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn client_telemetry_002_a_choice_whose_row_a_deletion_took_meanwhile_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    assert_eq!(choose(&api, &ada, false, None).await.status, 200);
    let db = api.database();
    // A deletion of the account that holds the consent's row first, so the choice finds the row
    // there, and waits for it.
    let mut deletion = db
        .holding(&format!(
            "SELECT 1 FROM telemetry_consents WHERE user_id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    let chosen = spawn_choice(&api, &ada);
    query_waits_on_a_lock(db, "SELECT id FROM telemetry_consents").await;
    execute_on(
        &mut deletion,
        &format!("DELETE FROM users WHERE id = '{}'", ada.user_id),
    )
    .await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, body) = chosen.await.unwrap();

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn client_telemetry_002_a_choice_whose_row_an_erasure_deletes_meanwhile_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    assert_eq!(choose(&api, &ada, false, None).await.status, 200);
    let db = api.database();
    // A deletion of the account whose erasure deleted the consent's row first, so the choice's
    // insert waits for it at the row's unique key.
    let mut deletion = db
        .holding(&format!(
            "DELETE FROM telemetry_consents WHERE user_id = '{}'",
            ada.user_id
        ))
        .await;

    let chosen = spawn_choice(&api, &ada);
    query_waits_on_a_lock(db, "INSERT INTO telemetry_consents").await;
    execute_on(
        &mut deletion,
        &format!("DELETE FROM users WHERE id = '{}'", ada.user_id),
    )
    .await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, body) = chosen.await.unwrap();

    // The row's foreign key refused it once the user was gone.
    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn client_telemetry_003_deleting_an_account_deletes_its_consent() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    assert_eq!(choose(&api, &ada, true, None).await.status, 200);
    assert_eq!(choose(&api, &grace, true, None).await.status, 200);

    let deleted = api
        .request(Method::DELETE, "/v1/users/me")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(deleted.status, 204);
    assert_eq!(rows(&api).await, [format!("{} true 1", grace.user_id)]);
}

#[tokio::test]
async fn client_telemetry_003_a_deletion_waits_for_a_choice_that_holds_the_consent_s_row() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    assert_eq!(choose(&api, &ada, true, None).await.status, 200);
    let db = api.database();
    // A choice that holds the consent's row, which it locks before it names the user.
    let mut choice = db
        .holding(&format!(
            "SELECT 1 FROM telemetry_consents WHERE user_id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    let mut deletion = api.spawn(Method::DELETE, "/v1/users/me", &ada.access, None);
    // The erasure waits for the row, before the deletion of the user's row, which waits for no lock.
    tokio::select! {
        () = query_waits_on_a_lock(db, "DELETE FROM telemetry_consents") => {}
        ended = &mut deletion => panic!("the deletion ended without waiting for the row: {ended:?}"),
    }
    execute_on(&mut choice, "COMMIT").await;
    let (status, body) = deletion.await.unwrap();

    assert_eq!(status, 204, "{body}");
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn client_telemetry_003_an_export_holds_the_consent_of_a_user_who_chose() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    assert_eq!(choose(&api, &ada, true, None).await.status, 200);
    assert_eq!(choose(&api, &ada, false, None).await.status, 200);
    let times = api
        .database()
        .owner_reads(
            r#"SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')
            || ' ' || to_char(updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')
            FROM telemetry_consents"#,
        )
        .await
        .remove(0);
    let (created_at, updated_at) = times.split_once(' ').unwrap();

    let mut archives = Vec::new();
    for user in [&ada, &grace] {
        let export = built(&api, user).await;
        let archive = api
            .get(&format!("/v1/users/me/exports/{export}/content"))
            .bearer(&user.access)
            .send()
            .await;
        assert_eq!(archive.status, 200);
        archives.push(document(
            &archive.body,
            "client_telemetry/telemetry_consents.json",
        ));
    }

    assert_eq!(
        archives[0],
        [json!({
            "consented": false,
            "created_at": created_at,
            "updated_at": updated_at,
        })]
    );
    assert!(archives[1].is_empty(), "{:?}", archives[1]);
}
