//! The session that a browser holds, which a request from it that starts a session ends (the auth
//! spec's "A user's sessions", AUTH-096).

use serde_json::{Value, json};

use crate::support::{
    Api,
    api::Response,
    api::reads,
    execute_on,
    providers::{gives, grant, identity, stand_in},
    query_waits_on_a_lock,
    second_factors::{self, add_app},
    upstream::Upstream,
    users::{self, PASSWORD, SignedIn, WEB},
};

/// Sends `body` to `path` from a browser at [`WEB`] that holds the cookies `cookies`.
async fn from_browser(api: &Api, path: &str, body: &Value, cookies: &str) -> Response {
    api.post(path, body)
        .header("origin", WEB)
        .header("cookie", cookies)
        .send()
        .await
}

/// Signs `email` in with [`PASSWORD`] from a browser at [`WEB`] that holds the cookies
/// `cookies`.
async fn sign_in(api: &Api, email: &str, cookies: &str) -> Response {
    from_browser(
        api,
        "/v1/sessions",
        &json!({ "email": email, "password": PASSWORD }),
        cookies,
    )
    .await
}

/// Registers `email`, and signs it in from a browser that holds no cookie: the user, and the
/// sign-in's response, whose cookies hold the session's tokens.
async fn in_a_browser(api: &Api, email: &str) -> (SignedIn, Response) {
    let user = users::register(api, email).await;
    let signed_in = sign_in(api, email, "").await;
    assert_eq!(signed_in.status, 201, "{}", signed_in.json());
    (user, signed_in)
}

/// The ID of the session that `response` answers.
fn session_of(response: &Response) -> String {
    response.json()["id"].as_str().unwrap().to_owned()
}

/// The IDs of the sessions that exist, in order.
async fn sessions(api: &Api) -> Vec<String> {
    reads(api, "SELECT id::text FROM sessions ORDER BY 1").await
}

/// The log, actor and session of each `session.ended` entry, the actor `-` when there is none,
/// in order.
async fn ended_entries(api: &Api) -> Vec<String> {
    reads(
        api,
        "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_id \
         FROM audit_entries WHERE action = 'session.ended' ORDER BY 1",
    )
    .await
}

#[tokio::test]
async fn auth_096_a_sign_in_from_a_browser_that_holds_a_session_ends_it() {
    let api = users::browsers_api().await;
    let (ada, first) = in_a_browser(&api, "ada@example.com").await;
    let held = session_of(&first);
    let mut signals = api.database().signals().await;

    let second = sign_in(&api, "ada@example.com", &users::cookies(&first)).await;

    assert_eq!(second.status, 201, "{}", second.json());
    let started = session_of(&second);
    assert_ne!(started, held);
    let mut left = vec![ada.session_id.clone(), started];
    left.sort();
    assert_eq!(sessions(&api).await, left);
    // The end is its user's own, through the full sign-out path.
    assert_eq!(
        ended_entries(&api).await,
        [format!("{0} {0} {held}", ada.user_id)]
    );
    assert_eq!(
        users::session_events(&api, "session.ended").await,
        [format!("{} {held}", ada.user_id)]
    );
    let closed: Vec<String> = signals
        .received()
        .await
        .iter()
        .map(|signal| signal["session_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(closed, [held]);
}

#[tokio::test]
async fn auth_096_the_access_cookie_names_the_session_or_else_the_refresh_cookie() {
    let api = users::browsers_api().await;
    let (ada, first) = in_a_browser(&api, "ada@example.com").await;
    let other = sign_in(&api, "ada@example.com", "").await;
    let access = format!(
        "__Host-access_token={}",
        users::cookie(&first, "access_token")
    );
    let refresh = format!(
        "__Host-refresh_token={}",
        users::cookie(&first, "refresh_token")
    );
    let others_refresh = format!(
        "__Host-refresh_token={}",
        users::cookie(&other, "refresh_token")
    );

    // The access cookie's session, where the refresh cookie names another.
    let by_access = sign_in(
        &api,
        "ada@example.com",
        &format!("{access}; {others_refresh}"),
    )
    .await;
    let after_access = sessions(&api).await;
    // The refresh cookie's session, where the access cookie names none.
    let by_refresh = sign_in(
        &api,
        "ada@example.com",
        &format!("__Host-access_token=no-session-s-token; {others_refresh}"),
    )
    .await;

    assert_eq!(by_access.status, 201, "{}", by_access.json());
    assert_eq!(by_refresh.status, 201, "{}", by_refresh.json());
    let mut expected = vec![
        ada.session_id.clone(),
        session_of(&other),
        session_of(&by_access),
    ];
    expected.sort();
    assert_eq!(after_access, expected);
    let mut expected = vec![
        ada.session_id.clone(),
        session_of(&by_access),
        session_of(&by_refresh),
    ];
    expected.sort();
    assert_eq!(sessions(&api).await, expected);
    // The refresh cookie of the first session, which the access cookie's end deleted, names
    // none.
    let none = sign_in(&api, "ada@example.com", &refresh).await;
    assert_eq!(none.status, 201, "{}", none.json());
    assert_eq!(sessions(&api).await.len(), 4);
}

#[tokio::test]
async fn auth_096_a_sign_in_as_another_user_ends_the_held_session_with_no_actor() {
    let api = users::browsers_api().await;
    let (ada, first) = in_a_browser(&api, "ada@example.com").await;
    let held = session_of(&first);
    let bob = users::register(&api, "bob@example.com").await;

    let second = sign_in(&api, "bob@example.com", &users::cookies(&first)).await;

    assert_eq!(second.status, 201, "{}", second.json());
    assert_eq!(second.json()["user_id"], bob.user_id.as_str());
    let mut left = vec![
        ada.session_id.clone(),
        bob.session_id.clone(),
        session_of(&second),
    ];
    left.sort();
    assert_eq!(sessions(&api).await, left);
    assert_eq!(
        ended_entries(&api).await,
        [format!("{} - {held}", ada.user_id)]
    );
    assert_eq!(
        users::session_events(&api, "session.ended").await,
        [format!("{} {held}", ada.user_id)]
    );
}

#[tokio::test]
async fn auth_096_a_registration_from_a_browser_that_holds_a_session_ends_it() {
    let api = users::browsers_api().await;
    let (ada, first) = in_a_browser(&api, "ada@example.com").await;
    let held = session_of(&first);

    let registered = from_browser(
        &api,
        "/v1/users",
        &users::registration("bob@example.com"),
        &users::cookies(&first),
    )
    .await;

    assert_eq!(registered.status, 201, "{}", registered.json());
    let mut left = vec![ada.session_id.clone(), session_of(&registered)];
    left.sort();
    assert_eq!(sessions(&api).await, left);
    assert_eq!(
        ended_entries(&api).await,
        [format!("{} - {held}", ada.user_id)]
    );
}

#[tokio::test]
async fn auth_096_a_challenge_ends_no_session_and_its_completion_ends_the_held_one() {
    let api = users::browsers_api().await;
    let (ada, first) = in_a_browser(&api, "ada@example.com").await;
    let held = session_of(&first);
    let app = add_app(&api, &ada).await;
    let cookies = users::cookies(&first);

    let challenged = sign_in(&api, "ada@example.com", &cookies).await;
    let during = sessions(&api).await;
    let code = second_factors::code(&api, &app.secret).await;
    let completed = from_browser(
        &api,
        "/v1/sessions/second-factor",
        &json!({ "code": code }),
        &format!("{cookies}; {}", users::cookies(&challenged)),
    )
    .await;

    assert_eq!(challenged.status, 200, "{}", challenged.json());
    assert!(during.contains(&held), "{during:?}");
    assert_eq!(completed.status, 201, "{}", completed.json());
    let mut left = vec![ada.session_id.clone(), session_of(&completed)];
    left.sort();
    assert_eq!(sessions(&api).await, left);
    assert_eq!(
        ended_entries(&api).await,
        [format!("{0} {0} {held}", ada.user_id)]
    );
}

#[tokio::test]
async fn auth_096_a_sign_in_through_a_provider_ends_the_held_session() {
    let upstream = Upstream::start().await;
    let api = Api::with(stand_in(&upstream).set("CORS_ALLOWED_ORIGINS", WEB)).await;
    let (ada, first) = in_a_browser(&api, "ada@example.com").await;
    let held = session_of(&first);
    // A subject that no account holds, with an address that none holds: the sign-in registers.
    gives(&upstream, identity("subject-1", "bob@example.com", true));

    let registered = from_browser(
        &api,
        "/v1/sessions/provider",
        &grant(),
        &users::cookies(&first),
    )
    .await;
    // The same subject again signs in to the account that the first registered, from the
    // browser that now holds that account's session.
    let signed_in = from_browser(
        &api,
        "/v1/sessions/provider",
        &grant(),
        &users::cookies(&registered),
    )
    .await;

    assert_eq!(registered.status, 201, "{}", registered.json());
    assert_eq!(signed_in.status, 201, "{}", signed_in.json());
    let mut left = vec![ada.session_id.clone(), session_of(&signed_in)];
    left.sort();
    assert_eq!(sessions(&api).await, left);
    let bob = registered.json()["user_id"].as_str().unwrap().to_owned();
    let mut ended = vec![
        format!("{} - {held}", ada.user_id),
        format!("{bob} {bob} {}", session_of(&registered)),
    ];
    ended.sort();
    assert_eq!(ended_entries(&api).await, ended);
}

#[tokio::test]
async fn auth_096_a_sign_in_that_fails_ends_no_session() {
    let api = users::browsers_api().await;
    let (_, first) = in_a_browser(&api, "ada@example.com").await;
    let before = sessions(&api).await;

    let failed = from_browser(
        &api,
        "/v1/sessions",
        &json!({ "email": "ada@example.com", "password": "not-the-password-at-all" }),
        &users::cookies(&first),
    )
    .await;

    assert_eq!(failed.status, 401, "{}", failed.json());
    assert_eq!(sessions(&api).await, before);
    assert!(ended_entries(&api).await.is_empty());
}

#[tokio::test]
async fn auth_096_cookies_that_name_no_session_or_come_without_an_origin_count_as_none() {
    let api = users::browsers_api().await;
    let (_, first) = in_a_browser(&api, "ada@example.com").await;
    let before = sessions(&api).await;
    let body = json!({ "email": "ada@example.com", "password": PASSWORD });

    let unknown = sign_in(
        &api,
        "ada@example.com",
        "__Host-access_token=no-session-s-token; __Host-refresh_token=nor-this-one",
    )
    .await;
    // A request that carries the session's cookies and no `Origin`, as no browser sends one.
    let without_origin = api
        .post("/v1/sessions", &body)
        .header("cookie", &users::cookies(&first))
        .send()
        .await;

    assert_eq!(unknown.status, 201, "{}", unknown.json());
    assert_eq!(without_origin.status, 201, "{}", without_origin.json());
    let mut expected = before;
    expected.extend([session_of(&unknown), session_of(&without_origin)]);
    expected.sort();
    assert_eq!(sessions(&api).await, expected);
    assert!(ended_entries(&api).await.is_empty());
}

#[tokio::test]
async fn auth_096_a_held_session_that_stopped_lasting_ends_too() {
    let api = users::browsers_api().await;
    let (ada, first) = in_a_browser(&api, "ada@example.com").await;
    let held = session_of(&first);
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET access_token_expires_at = now() - interval '1 hour', \
             refresh_token_expires_at = now() - interval '1 hour', \
             expires_at = now() - interval '1 hour' WHERE id = '{held}'"
        ))
        .await;
    let refresh = format!(
        "__Host-refresh_token={}",
        users::cookie(&first, "refresh_token")
    );

    let by_access = sign_in(&api, "ada@example.com", &users::cookies(&first)).await;
    let after_access = sessions(&api).await;
    // Another session that stopped lasting, which the refresh cookie alone names.
    let other = sign_in(&api, "ada@example.com", "").await;
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET access_token_expires_at = now() - interval '1 hour', \
             refresh_token_expires_at = now() - interval '1 hour', \
             expires_at = now() - interval '1 hour' WHERE id = '{}'",
            session_of(&other)
        ))
        .await;
    let others_refresh = format!(
        "__Host-refresh_token={}",
        users::cookie(&other, "refresh_token")
    );
    let by_refresh = sign_in(&api, "ada@example.com", &others_refresh).await;

    assert_eq!(by_access.status, 201, "{}", by_access.json());
    let mut left = vec![ada.session_id.clone(), session_of(&by_access)];
    left.sort();
    assert_eq!(after_access, left);
    assert_eq!(by_refresh.status, 201, "{}", by_refresh.json());
    let mut left = vec![
        ada.session_id.clone(),
        session_of(&by_access),
        session_of(&by_refresh),
    ];
    left.sort();
    assert_eq!(sessions(&api).await, left);
    // The first session's own refresh cookie names none any more.
    assert_eq!(sign_in(&api, "ada@example.com", &refresh).await.status, 201);
    assert_eq!(sessions(&api).await.len(), 4);
}

#[tokio::test]
async fn auth_096_a_held_session_that_is_the_one_on_the_device_ends_once() {
    let api = users::browsers_api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let on_device = json!({
        "email": "ada@example.com",
        "password": PASSWORD,
        "device_id": "browser-1"
    });
    let first = from_browser(&api, "/v1/sessions", &on_device, "").await;
    let held = session_of(&first);

    // The session that the cookies name is the one that the user has on the device.
    let second = from_browser(&api, "/v1/sessions", &on_device, &users::cookies(&first)).await;

    assert_eq!(second.status, 201, "{}", second.json());
    let mut left = vec![ada.session_id.clone(), session_of(&second)];
    left.sort();
    assert_eq!(sessions(&api).await, left);
    assert_eq!(
        ended_entries(&api).await,
        [format!("{0} {0} {held}", ada.user_id)]
    );
}

#[tokio::test]
async fn auth_096_a_request_ends_the_sessions_it_shows_in_the_order_of_their_ids() {
    let api = users::browsers_api().await;
    // Bob's session, which the browser holds, is older than Ada's on the device, so its ID
    // comes first.
    let (_, bobs) = in_a_browser(&api, "bob@example.com").await;
    let held = session_of(&bobs);
    users::register(&api, "ada@example.com").await;
    let on_device = json!({
        "email": "ada@example.com",
        "password": PASSWORD,
        "device_id": "browser-1"
    });
    let adas = from_browser(&api, "/v1/sessions", &on_device, "").await;
    let device = session_of(&adas);
    assert!(held < device, "{held} then {device}");
    // Another request holds the held session's row.
    let mut holder = api
        .database()
        .holding(&format!(
            "SELECT 1 FROM sessions WHERE id = '{held}' FOR UPDATE"
        ))
        .await;

    let sign_in = tokio::spawn({
        let api = api.clone();
        let cookies = users::cookies(&bobs);
        async move { from_browser(&api, "/v1/sessions", &on_device, &cookies).await }
    });
    // The sign-in waits for the held session's row, having deleted no other: the row of the
    // session on its device is free.
    query_waits_on_a_lock(api.database(), "DELETE FROM sessions").await;
    let free = api
        .database()
        .owner_reads(&format!(
            "SELECT id::text FROM sessions WHERE id = '{device}' FOR UPDATE NOWAIT"
        ))
        .await;
    execute_on(&mut holder, "COMMIT").await;
    let sign_in = sign_in.await.unwrap();

    assert_eq!(free, [device.as_str()]);
    assert_eq!(sign_in.status, 201, "{}", sign_in.json());
    let left = sessions(&api).await;
    assert!(!left.contains(&held) && !left.contains(&device), "{left:?}");
}
