//! A user's sessions: their devices' names, their list, ending one or every other, the events
//! that tell the user's streams a session started or ended, and the hourly job that deletes those
//! that stopped lasting, with the answers to their tokens once it has (AUTH-042 to AUTH-047,
//! AUTH-009, AUTH-011).

use serde_json::{Value, json};

use crate::support::{
    Api, Options,
    api::Response,
    execute_on, lists, queries_wait_on_locks, streams,
    users::{self, PASSWORD, SignedIn, browser_sign_in, browsers_api, cookie},
};

/// Signs ada@example.com in with `body`'s other fields, such as a device's ID and name, and
/// returns the new session.
async fn sign_in_with(api: &Api, fields: Value) -> SignedIn {
    let mut body = json!({ "email": "ada@example.com", "password": PASSWORD });
    body.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    let response = api.post("/v1/sessions", &body).send().await;
    assert_eq!(response.status, 201, "{}", response.json());
    SignedIn::from(&response)
}

/// Ends `user`'s session `id`.
async fn end(api: &Api, user: &SignedIn, id: &str) -> Response {
    api.request(http::Method::DELETE, &format!("/v1/users/me/sessions/{id}"))
        .bearer(&user.access)
        .send()
        .await
}

/// Ends every session of `user`'s but their own.
async fn end_others(api: &Api, user: &SignedIn) -> Response {
    api.request(http::Method::DELETE, "/v1/users/me/sessions/others")
        .bearer(&user.access)
        .send()
        .await
}

/// Sends, in a task of its own, with the access token `access`, a `POST` of `body` to `path`, or a
/// `DELETE` of `path` without one, and gives its status and its body's JSON, or null for an empty
/// body.
fn spawn_request(
    api: &Api,
    path: &str,
    access: &str,
    body: Option<Value>,
) -> tokio::task::JoinHandle<(http::StatusCode, Value)> {
    let (api, path, access) = (api.clone(), path.to_owned(), access.to_owned());
    tokio::spawn(async move {
        let request = match body {
            Some(body) => api.post(&path, &body),
            None => api.request(http::Method::DELETE, &path),
        };
        let response = request.bearer(&access).send().await;
        let body = if response.body.is_empty() {
            Value::Null
        } else {
            response.json()
        };
        (response.status, body)
    })
}

/// The IDs of `user`'s sessions, sorted.
async fn sessions_of(api: &Api, user: &SignedIn) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT id::text FROM sessions WHERE user_id = '{}' ORDER BY 1",
            user.user_id
        ))
        .await
}

/// Refreshes the session whose refresh token is `token`.
async fn refresh(api: &Api, token: &str) -> Response {
    api.post("/v1/sessions/refresh", &json!({ "refresh_token": token }))
        .send()
        .await
}

/// Whether `user`'s access token still authenticates a request.
async fn signed_in(api: &Api, user: &SignedIn) -> bool {
    let response = api.get("/v1/users/me").bearer(&user.access).send().await;
    assert!(
        response.status == 200 || response.status == 401,
        "{}",
        response.status
    );
    response.status == 200
}

/// The IDs of `user`'s sessions that `session.ended` entries of their log name, sorted.
async fn ended_entries(api: &Api, user: &SignedIn) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT resource_id::text FROM audit_entries \
             WHERE action = 'session.ended' AND user_id = '{}' ORDER BY 1",
            user.user_id
        ))
        .await
}

/// What `users::session_events` gives for one event of `user`'s about each of `sessions`.
fn of(user: &SignedIn, sessions: &[&SignedIn]) -> Vec<String> {
    let mut events: Vec<String> = sessions
        .iter()
        .map(|session| format!("{} {}", user.user_id, session.session_id))
        .collect();
    events.sort();
    events
}

#[tokio::test]
async fn auth_042_a_registration_s_and_a_sign_in_s_device_name_is_kept_normalized() {
    let api = Api::new().await;
    let mut body = users::registration("ada@example.com");
    body["device_name"] = json!("  Ada's \t laptop \n");
    let registered = api.post("/v1/users", &body).send().await;
    let ada = SignedIn::from(&registered);
    users::verified(&api, &ada.user_id).await;
    let phone = sign_in_with(
        &api,
        json!({ "device_id": "phone_1", "device_name": "Ada's phone" }),
    )
    .await;

    let current = api
        .get("/v1/users/me/sessions/current")
        .bearer(&phone.access)
        .send()
        .await;
    // A sign-in that replaces its device's session gives the new one its own name, or none.
    let replaced = sign_in_with(&api, json!({ "device_id": "phone_1" })).await;
    let unnamed = api
        .get("/v1/users/me/sessions/current")
        .bearer(&replaced.access)
        .send()
        .await;

    assert_eq!(registered.json()["device_name"], "Ada's laptop");
    assert_eq!(current.json()["device_name"], "Ada's phone");
    assert_eq!(unnamed.json()["device_name"], json!(null));
    let stored = api
        .database()
        .owner_reads(&format!(
            "SELECT coalesce(device_name, '-') FROM sessions WHERE user_id = '{}' \
             ORDER BY created_at",
            ada.user_id
        ))
        .await;
    assert_eq!(stored, ["Ada's laptop", "-"]);
}

#[tokio::test]
async fn auth_042_a_device_name_without_a_character_or_over_100_answers_400() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let blank = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD, "device_name": " \t " }),
        )
        .send()
        .await;
    // A name longer than the document allows is malformed input, sent raw.
    let mut body = users::registration("grace@example.com");
    body["device_name"] = json!("é".repeat(101));
    let long = api
        .request(http::Method::POST, "/v1/users")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await;

    for (refused, code) in [(&blank, "required"), (&long, "too_long")] {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "request.invalid_fields");
        assert_eq!(refused.json()["errors"][0]["field"], "/device_name");
        assert_eq!(refused.json()["errors"][0]["code"], code);
    }
    let sessions = api
        .database()
        .owner_reads("SELECT count(*)::text FROM sessions")
        .await;
    assert_eq!(sessions, ["1"]);
}

#[tokio::test]
async fn auth_043_the_caller_lists_their_sessions_that_last_and_no_other_user_s() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let phone = sign_in_with(
        &api,
        json!({ "device_id": "phone_1", "device_name": "Ada's phone" }),
    )
    .await;
    let expired = users::new_session(&api, "ada@example.com").await;
    let ended = users::new_session(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    db.owner_executes(&format!(
        "UPDATE sessions SET created_at = created_at - interval '1 minute' WHERE id = '{}'; \
         UPDATE sessions SET refresh_token_expires_at = now() WHERE id = '{}'; \
         UPDATE sessions SET expires_at = now() WHERE id = '{}'",
        ada.session_id, expired.session_id, ended.session_id
    ))
    .await;

    let list = api
        .get("/v1/users/me/sessions")
        .bearer(&phone.access)
        .send()
        .await;

    assert_eq!(list.status, 200);
    let body = list.json();
    assert_eq!(
        lists::field(&body, "id"),
        [phone.session_id.as_str(), ada.session_id.as_str()]
    );
    assert_eq!(body["next_cursor"], json!(null));
    let (newest, oldest) = (&body["items"][0], &body["items"][1]);
    assert_eq!(newest["current"], true);
    assert_eq!(oldest["current"], false);
    assert_eq!(newest["device_id"], "phone_1");
    assert_eq!(newest["device_name"], "Ada's phone");
    assert_eq!(oldest["device_name"], json!(null));
    for field in [
        "created_at",
        "access_token_expires_at",
        "refresh_token_expires_at",
        "expires_at",
        "authenticated_at",
    ] {
        assert!(newest[field].is_string(), "{field}");
    }
    for item in body["items"].as_array().unwrap() {
        assert!(item.get("access_token").is_none());
        assert!(item.get("refresh_token").is_none());
        assert!(item.get("user_id").is_none());
    }
    let theirs = api
        .get("/v1/users/me/sessions")
        .bearer(&grace.access)
        .send()
        .await;
    assert_eq!(
        lists::field(&theirs.json(), "id"),
        [grace.session_id.as_str()]
    );
}

#[tokio::test]
async fn auth_043_paging_returns_each_session_once_across_a_shared_creation_time() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut started = vec![ada.session_id.clone()];
    for _ in 0..2 {
        started.push(users::new_session(&api, "ada@example.com").await.session_id);
    }
    api.database()
        .owner_executes("UPDATE sessions SET created_at = date_trunc('second', now())")
        .await;

    let pages = lists::pages(&api, &ada, "/v1/users/me/sessions?limit=1", "id").await;

    assert_eq!(pages.len(), 3);
    let mut listed: Vec<String> = pages.into_iter().flatten().collect();
    listed.sort();
    started.sort();
    assert_eq!(listed, started);
}

#[tokio::test]
async fn auth_043_a_search_pages_through_its_matches() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut laptops = Vec::new();
    for name in ["Ada's LAPTOP", "Work laptop", "Phone", "laptop at home"] {
        let session = sign_in_with(&api, json!({ "device_name": name })).await;
        if name.to_lowercase().contains("laptop") {
            laptops.push(session.session_id);
        }
    }

    let pages = lists::pages(
        &api,
        &ada,
        "/v1/users/me/sessions?search=Laptop&limit=2",
        "id",
    )
    .await;

    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].len(), 2);
    let mut found: Vec<String> = pages.into_iter().flatten().collect();
    found.sort();
    laptops.sort();
    assert_eq!(found, laptops);
}

#[tokio::test]
async fn auth_044_ending_another_session_needs_a_recent_proof_then_ends_it_and_its_streams() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let phone = users::new_session(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &phone).await;
    users::authenticated_ago(&api, &ada, 10).await;

    let refused = end(&api, &ada, &phone.session_id).await;
    let still = signed_in(&api, &phone).await;
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET authenticated_at = now() WHERE id = '{}'",
            ada.session_id
        ))
        .await;
    let mut signals = api.database().signals().await;
    let ended = end(&api, &ada, &phone.session_id).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.reauthentication_required");
    assert!(still);
    assert_eq!(ended.status, 204);
    assert!(ended.body.is_empty());
    assert!(ended.headers("set-cookie").is_empty());
    assert!(!signed_in(&api, &phone).await);
    assert!(signed_in(&api, &ada).await);
    assert_eq!(ended_entries(&api, &ada).await, [phone.session_id.as_str()]);
    let received: Vec<Value> = signals.received().await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["kind"], "session_ended");
    assert_eq!(received[0]["session_id"], phone.session_id.as_str());
    let (_, reason) = stream.closed().await;
    assert_eq!(reason, "session_ended");
}

#[tokio::test]
async fn auth_044_ending_one_s_own_session_by_its_id_signs_out_without_a_recent_proof() {
    let api = browsers_api().await;
    let signed_in_browser = browser_sign_in(&api).await;
    let access = cookie(&signed_in_browser, "access_token");
    let id = signed_in_browser.json()["id"].as_str().unwrap().to_owned();
    api.database()
        .owner_executes("UPDATE sessions SET authenticated_at = now() - interval '10 minutes'")
        .await;
    let mut signals = api.database().signals().await;

    let ended = api
        .request(http::Method::DELETE, &format!("/v1/users/me/sessions/{id}"))
        .bearer(&access)
        .send()
        .await;

    assert_eq!(ended.status, 204);
    let cleared = ended.headers("set-cookie");
    for name in ["access_token", "refresh_token"] {
        assert!(
            cleared.contains(
                &format!("__Host-{name}=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict")
                    .as_str()
            ),
            "{cleared:?}"
        );
    }
    let after = api.get("/v1/users/me").bearer(&access).send().await;
    assert_eq!(after.status, 401);
    let received: Vec<Value> = signals.received().await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["kind"], "session_ended");
    assert_eq!(received[0]["session_id"], id.as_str());
    let entries = api
        .database()
        .owner_reads("SELECT resource_id::text FROM audit_entries WHERE action = 'session.ended'")
        .await;
    assert_eq!(entries, [id]);
}

#[tokio::test]
async fn auth_044_an_id_that_names_none_of_the_caller_s_sessions_answers_404_once_proved() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let unknown = "0195f0a4-0000-7000-8000-000000000000";

    let mut answers = Vec::new();
    for id in [grace.session_id.as_str(), unknown, "not-a-uuid"] {
        answers.push(end(&api, &ada, id).await);
    }
    users::authenticated_ago(&api, &ada, 10).await;
    let unproved = end(&api, &ada, "not-a-uuid").await;

    for answer in &answers {
        assert_eq!(answer.status, 404);
        assert_eq!(answer.json()["code"], "auth.session_not_found");
    }
    // An ID that isn't a UUID answers as one that names no session does, after the proof.
    assert_eq!(unproved.status, 403);
    assert_eq!(unproved.json()["code"], "auth.reauthentication_required");
    assert!(signed_in(&api, &grace).await);
    assert!(signed_in(&api, &ada).await);
    assert!(ended_entries(&api, &ada).await.is_empty());
}

#[tokio::test]
async fn auth_044_ending_a_session_that_stopped_lasting_deletes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let old = users::new_session(&api, "ada@example.com").await;
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET refresh_token_expires_at = now() WHERE id = '{}'",
            old.session_id
        ))
        .await;

    let ended = end(&api, &ada, &old.session_id).await;
    let again = end(&api, &ada, &old.session_id).await;

    assert_eq!(ended.status, 204);
    assert_eq!(again.status, 404);
    let left = api
        .database()
        .owner_reads(&format!(
            "SELECT count(*)::text FROM sessions WHERE id = '{}'",
            old.session_id
        ))
        .await;
    assert_eq!(left, ["0"]);
}

#[tokio::test]
async fn auth_045_ending_the_other_sessions_needs_a_recent_proof_and_keeps_the_caller_s() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let phone = users::new_session(&api, "ada@example.com").await;
    let expired = users::new_session(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET refresh_token_expires_at = now() WHERE id = '{}'",
            expired.session_id
        ))
        .await;
    users::authenticated_ago(&api, &ada, 10).await;

    let refused = end_others(&api, &ada).await;
    let still = signed_in(&api, &phone).await;
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET authenticated_at = now() WHERE id = '{}'",
            ada.session_id
        ))
        .await;
    let mut signals = api.database().signals().await;
    let ended = end_others(&api, &ada).await;
    let again = end_others(&api, &ada).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.reauthentication_required");
    assert!(still);
    assert_eq!(ended.status, 204);
    assert_eq!(again.status, 204);
    assert!(!signed_in(&api, &phone).await);
    assert!(signed_in(&api, &ada).await);
    assert!(signed_in(&api, &grace).await);
    let mut gone = vec![phone.session_id.clone(), expired.session_id.clone()];
    gone.sort();
    assert_eq!(ended_entries(&api, &ada).await, gone);
    let received: Vec<Value> = signals.received().await;
    assert_eq!(received.len(), 2);
    let left = api
        .database()
        .owner_reads(&format!(
            "SELECT id::text FROM sessions WHERE user_id = '{}'",
            ada.user_id
        ))
        .await;
    assert_eq!(left, [ada.session_id.as_str()]);
}

#[tokio::test]
async fn auth_044_an_account_that_must_verify_signs_out_through_current_alone() {
    let api = Api::new().await;
    let ada = users::register_unverified(&api, "ada@example.com").await;

    let by_id = end(&api, &ada, &ada.session_id).await;
    let still = api
        .get("/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await;
    let current = end(&api, &ada, "current").await;

    assert_eq!(by_id.status, 403);
    assert_eq!(by_id.json()["code"], "request.email_unverified");
    assert_eq!(still.status, 200);
    assert_eq!(current.status, 204);
    assert!(sessions_of(&api, &ada).await.is_empty());
}

#[tokio::test]
async fn auth_045_two_ends_of_each_other_s_sessions_take_turns_and_the_second_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let phone = users::new_session(&api, "ada@example.com").await;
    let tablet = users::new_session(&api, "ada@example.com").await;
    let db = api.database();
    // A lock on the tablet's session holds the first end at its delete, once it has its proof.
    let mut holder = db
        .holding(&format!(
            "SELECT 1 FROM sessions WHERE id = '{}' FOR UPDATE",
            tablet.session_id
        ))
        .await;
    let others = "/v1/users/me/sessions/others";

    let first = spawn_request(&api, others, &ada.access, None);
    queries_wait_on_locks(db, 1).await;
    let second = spawn_request(&api, others, &phone.access, None);
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut holder, "COMMIT").await;
    let (first, second) = (first.await.unwrap(), second.await.unwrap());

    assert_eq!(first.0, 204);
    // The phone's turn came once Ada's end had ended its session.
    assert_eq!(second.0, 401);
    assert_eq!(second.1["code"], "request.unauthenticated");
    assert_eq!(sessions_of(&api, &ada).await, [ada.session_id.as_str()]);
}

#[tokio::test]
async fn auth_045_a_change_of_the_password_takes_its_turn_after_an_end_of_its_session() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let phone = users::new_session(&api, "ada@example.com").await;
    let tablet = users::new_session(&api, "ada@example.com").await;
    let db = api.database();
    let mut holder = db
        .holding(&format!(
            "SELECT 1 FROM sessions WHERE id = '{}' FOR UPDATE",
            tablet.session_id
        ))
        .await;
    let change = json!({ "current_password": PASSWORD, "password": "lantern-orchard-58-brisk" });

    let first = spawn_request(&api, "/v1/users/me/sessions/others", &ada.access, None);
    queries_wait_on_locks(db, 1).await;
    let second = spawn_request(
        &api,
        "/v1/users/me/password/change",
        &phone.access,
        Some(change),
    );
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut holder, "COMMIT").await;
    let (first, second) = (first.await.unwrap(), second.await.unwrap());

    assert_eq!(first.0, 204);
    assert_eq!(second.0, 401);
    assert_eq!(second.1["code"], "request.unauthenticated");
    assert_eq!(sessions_of(&api, &ada).await, [ada.session_id.as_str()]);
    // The change rolled back: the password is the one it had.
    let signed_in = users::sign_in(&api, "ada@example.com", PASSWORD).await;
    assert_eq!(signed_in.status, 201);
}

#[tokio::test]
async fn auth_044_ending_another_session_takes_its_turn_after_a_deletion_of_the_account() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let phone = users::new_session(&api, "ada@example.com").await;
    let db = api.database();
    // A deletion of the account that holds the user's row, and the sessions with it.
    let mut deletion = db
        .holding(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    let path = format!("/v1/users/me/sessions/{}", phone.session_id);
    let ended = spawn_request(&api, &path, &ada.access, None);
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, body) = ended.await.unwrap();

    // Its turn came once the user was gone, and their sessions with them.
    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    let recorded = db
        .owner_reads("SELECT count(*)::text FROM audit_entries WHERE action = 'session.ended'")
        .await;
    assert_eq!(recorded, ["0"]);
}

#[tokio::test]
async fn data_20_a_change_of_the_password_and_a_sign_in_on_its_device_never_deadlock() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let phone = sign_in_with(&api, json!({ "device_id": "phone_1" })).await;
    let db = api.database();
    // A sign-in on the phone's device: it locks the password identity, then deletes the session
    // that its device holds.
    let mut sign_in = db
        .holding(&format!(
            "SELECT 1 FROM identities WHERE user_id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;
    let change = json!({ "current_password": PASSWORD, "password": "lantern-orchard-58-brisk" });

    let changed = spawn_request(
        &api,
        "/v1/users/me/password/change",
        &phone.access,
        Some(change),
    );
    queries_wait_on_locks(db, 1).await;
    // The change waits for the identity, holding no session's row, so this delete waits for
    // nothing.
    execute_on(
        &mut sign_in,
        &format!("DELETE FROM sessions WHERE id = '{}'", phone.session_id),
    )
    .await;
    execute_on(&mut sign_in, "COMMIT").await;
    let (status, body) = changed.await.unwrap();

    // The sign-in ended the phone's session first, so the change finds it gone.
    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert_eq!(
        users::sign_in(&api, "ada@example.com", PASSWORD)
            .await
            .status,
        201
    );
    assert!(signed_in(&api, &ada).await);
}

#[tokio::test]
async fn auth_046_a_session_s_start_and_end_reach_the_user_s_other_streams() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;

    let phone = users::new_session(&api, "ada@example.com").await;
    let started = stream.event().await;
    let signed_out = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/current")
        .bearer(&phone.access)
        .send()
        .await;
    let ended = stream.event().await;

    assert_eq!(signed_out.status, 204);
    assert_eq!(started.event.as_deref(), Some("session.created"));
    assert_eq!(
        started.payload(),
        &json!({ "session_id": phone.session_id })
    );
    assert_eq!(ended.event.as_deref(), Some("session.ended"));
    assert_eq!(ended.payload(), &json!({ "session_id": phone.session_id }));
}

#[tokio::test]
async fn auth_046_each_way_a_session_starts_or_ends_publishes_and_a_refresh_publishes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let tablet = sign_in_with(&api, json!({ "device_id": "tablet_1" })).await;
    // A sign-in on the tablet's device replaces its session.
    let replaced = sign_in_with(&api, json!({ "device_id": "tablet_1" })).await;
    let reused = users::new_session(&api, "ada@example.com").await;
    let before = api
        .database()
        .owner_reads("SELECT count(*)::text FROM events")
        .await;
    assert_eq!(refresh(&api, &reused.refresh).await.status, 200);
    let after_refresh = api
        .database()
        .owner_reads("SELECT count(*)::text FROM events")
        .await;
    // Presenting the rotated token again ends its session (AUTH-012).
    assert_eq!(refresh(&api, &reused.refresh).await.status, 401);
    let other = users::new_session(&api, "ada@example.com").await;
    assert_eq!(end(&api, &ada, &other.session_id).await.status, 204);
    let rest = users::new_session(&api, "ada@example.com").await;
    assert_eq!(end_others(&api, &ada).await.status, 204);

    assert_eq!(before, after_refresh);
    assert_eq!(
        users::session_events(&api, "session.created").await,
        of(&ada, &[&ada, &tablet, &replaced, &reused, &other, &rest])
    );
    assert_eq!(
        users::session_events(&api, "session.ended").await,
        of(&ada, &[&tablet, &reused, &other, &replaced, &rest])
    );
}

#[tokio::test]
async fn auth_046_a_deletion_of_the_account_publishes_no_session_s_end() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::new_session(&api, "ada@example.com").await;

    let deleted = api
        .request(http::Method::DELETE, "/v1/users/me")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(deleted.status, 204);
    let events = api
        .database()
        .owner_reads("SELECT name FROM events WHERE name LIKE 'session.%'")
        .await;
    assert!(events.is_empty(), "{events:?}");
}

/// Makes `user`'s session one that stopped lasting an hour ago: its tokens and its absolute
/// lifetime expired, a time in the past that only a test sets (TEST-09).
async fn stopped_lasting(api: &Api, user: &SignedIn) {
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET access_token_expires_at = now() - interval '1 hour', \
             refresh_token_expires_at = now() - interval '1 hour', \
             expires_at = now() - interval '1 hour' WHERE id = '{}'",
            user.session_id
        ))
        .await;
}

#[tokio::test]
async fn auth_047_the_hourly_job_deletes_every_user_s_sessions_that_stopped_lasting() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let old = sign_in_with(&api, json!({ "device_name": "Ada's old phone" })).await;
    let grace = users::register(&api, "grace@example.com").await;
    let gone = users::new_session(&api, "grace@example.com").await;
    let idle = users::new_session(&api, "grace@example.com").await;
    let db = api.database();
    stopped_lasting(&api, &old).await;
    // A session whose access token alone expired still lasts (AUTH-018).
    db.owner_executes(&format!(
        "UPDATE sessions SET access_token_expires_at = now() - interval '1 second' \
         WHERE id = '{}'",
        idle.session_id
    ))
    .await;
    // A session whose refresh token alone expired has stopped lasting too (AUTH-018).
    db.owner_executes(&format!(
        "UPDATE sessions SET refresh_token_expires_at = now() - interval '1 second' \
         WHERE id = '{}'",
        gone.session_id
    ))
    .await;
    db.owner_executes("DELETE FROM events").await;
    let entries = "SELECT count(*)::text FROM audit_entries";
    let before = db.owner_reads(entries).await;
    let mut signals = db.signals().await;

    api.run_periodic("auth.expire_sessions").await;

    let mut left = vec![
        ada.session_id.clone(),
        grace.session_id.clone(),
        idle.session_id.clone(),
    ];
    left.sort();
    assert_eq!(
        db.owner_reads("SELECT id::text FROM sessions ORDER BY 1")
            .await,
        left
    );
    // Scheduled maintenance records, publishes and signals nothing.
    assert_eq!(db.owner_reads(entries).await, before);
    assert!(db.owner_reads("SELECT name FROM events").await.is_empty());
    assert!(signals.received().await.is_empty());
}

#[tokio::test]
async fn auth_047_the_job_deletes_1500_sessions_and_a_second_run_nothing_more() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    // 1,500 sessions of Ada's that stopped lasting, more than one batch of 1,000.
    db.owner_executes(&format!(
        "INSERT INTO sessions (user_id, access_token_hash, refresh_token_hash, \
         access_token_expires_at, refresh_token_expires_at, expires_at) \
         SELECT '{}', sha256(('access ' || n)::bytea), sha256(('refresh ' || n)::bytea), \
         now() - interval '1 hour', now() - interval '1 hour', now() + interval '1 day' \
         FROM generate_series(1, 1500) AS n",
        ada.user_id
    ))
    .await;

    api.run_periodic("auth.expire_sessions").await;
    let once = sessions_of(&api, &ada).await;
    api.run_periodic("auth.expire_sessions").await;

    assert_eq!(once, [ada.session_id.as_str()]);
    assert_eq!(sessions_of(&api, &ada).await, once);
}

#[tokio::test]
async fn auth_047_a_session_that_a_request_holds_is_left_for_the_next_run() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let held = users::new_session(&api, "ada@example.com").await;
    let free = users::new_session(&api, "ada@example.com").await;
    stopped_lasting(&api, &held).await;
    stopped_lasting(&api, &free).await;
    let db = api.database();
    let mut request = db
        .holding(&format!(
            "SELECT 1 FROM sessions WHERE id = '{}' FOR UPDATE",
            held.session_id
        ))
        .await;

    // The run waits for no lock, so it ends while the row stays held.
    api.run_periodic("auth.expire_sessions").await;
    let during = sessions_of(&api, &ada).await;
    execute_on(&mut request, "COMMIT").await;
    api.run_periodic("auth.expire_sessions").await;

    let mut kept = vec![ada.session_id.clone(), held.session_id.clone()];
    kept.sort();
    assert_eq!(during, kept);
    assert_eq!(sessions_of(&api, &ada).await, [ada.session_id.as_str()]);
}

#[tokio::test]
async fn auth_011_an_expired_refresh_token_whose_session_the_job_deleted_answers_as_an_unknown_one()
{
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let old = users::new_session(&api, "ada@example.com").await;
    stopped_lasting(&api, &old).await;

    let before = refresh(&api, &old.refresh).await;
    api.run_periodic("auth.expire_sessions").await;
    let after = refresh(&api, &old.refresh).await;

    assert_eq!(before.status, 401);
    assert_eq!(before.json()["code"], "auth.session_expired");
    assert_eq!(after.status, 401);
    assert_eq!(after.json()["code"], "auth.invalid_refresh_token");
}

#[tokio::test]
async fn auth_009_an_expired_access_token_whose_session_the_job_deleted_answers_as_an_unknown_one()
{
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let old = users::new_session(&api, "ada@example.com").await;
    stopped_lasting(&api, &old).await;

    let before = api.get("/v1/users/me").bearer(&old.access).send().await;
    api.run_periodic("auth.expire_sessions").await;
    let after = api.get("/v1/users/me").bearer(&old.access).send().await;

    assert_eq!(before.status, 401);
    assert_eq!(before.json()["code"], "request.access_token_expired");
    assert_eq!(after.status, 401);
    assert_eq!(after.json()["code"], "request.unauthenticated");
}

#[tokio::test]
async fn auth_007_no_access_token_outlives_its_refresh_token_whatever_the_settings() {
    let options = Options::default()
        .set("ACCESS_TOKEN_SECONDS", 7_200)
        .set("REFRESH_TOKEN_SECONDS", 3_600);
    let api = Api::with(options).await;

    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await
        .json();
    let refreshed = refresh(&api, registered["refresh_token"].as_str().unwrap())
        .await
        .json();

    for session in [&registered, &refreshed] {
        assert_eq!(
            session["access_token_expires_at"], session["refresh_token_expires_at"],
            "{session}"
        );
    }
}
