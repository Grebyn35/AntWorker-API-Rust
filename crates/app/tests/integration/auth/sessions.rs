//! Authenticated requests, refresh, cookies and signing out (AUTH-009 to AUTH-013, AUTH-015,
//! AUTH-016).

use serde_json::{Value, json};

use crate::support::{
    Api,
    api::Response,
    execute_on,
    providers::{self, gives, grant, identity},
    queries_wait_on_locks, queries_waiting_on_locks, second_factors,
    upstream::Upstream,
    users::{self, PASSWORD, WEB, browser_sign_in, browsers_api, cookie, expires},
    workspaces,
};

#[tokio::test]
async fn auth_009_a_request_without_a_valid_access_token_answers_401() {
    let api = Api::new().await;
    for request in [
        api.get("/v1/users/me"),
        api.get("/v1/users/me").bearer("not-a-token"),
        api.get("/v1/users/me")
            .header("authorization", "Basic YWRhOnB3"),
        api.get("/v1/users/me")
            .header("cookie", "__Host-access_token=unknown"),
    ] {
        let response = request.send().await;

        assert_eq!(response.status, 401);
        assert_eq!(response.json()["code"], "request.unauthenticated");
    }
}

#[tokio::test]
async fn auth_009_an_expired_access_token_answers_its_own_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    api.database()
        .owner_executes("UPDATE sessions SET access_token_expires_at = now() - interval '1 second'")
        .await;

    let response = api.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "request.access_token_expired");
}

#[tokio::test]
async fn auth_009_a_browser_s_access_cookie_authenticates_without_an_authorization_header() {
    let api = browsers_api().await;
    let signed_in = browser_sign_in(&api).await;
    let access = cookie(&signed_in, "access_token");

    let response = api
        .get("/v1/users/me")
        .header("cookie", &format!("__Host-access_token={access}"))
        .send()
        .await;

    assert_eq!(response.status, 200);
    assert_eq!(response.json()["email"], "ada@example.com");
}

#[tokio::test]
async fn auth_009_an_authorization_header_that_holds_no_bearer_token_leaves_the_access_cookie_unread()
 {
    let api = browsers_api().await;
    let signed_in = browser_sign_in(&api).await;
    let access = cookie(&signed_in, "access_token");

    let response = api
        .get("/v1/users/me")
        .header("authorization", "Basic YWRhOnB3")
        .header("cookie", &format!("__Host-access_token={access}"))
        .send()
        .await;

    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "request.unauthenticated");
}

#[tokio::test]
async fn auth_009_a_request_whose_session_ended_while_it_waited_for_a_lock_completes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let phone = users::new_session(&api, "ada@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Analytical", &[(&ada.user_id, "owner")]).await;
    // The rename waits for the workspace's row, which the end of a session doesn't lock.
    let mut held = db
        .holding(&format!(
            "SELECT 1 FROM workspaces WHERE id = '{workspace}' FOR UPDATE"
        ))
        .await;

    let rename = tokio::spawn({
        let api = api.clone();
        let access = ada.access.clone();
        let path = format!("/v1/workspaces/{workspace}");
        async move {
            api.patch(&path, &json!({ "name": "Engines" }))
                .bearer(&access)
                .send()
                .await
        }
    });
    queries_wait_on_locks(db, 1).await;
    let ended = api
        .request(
            http::Method::DELETE,
            &format!("/v1/users/me/sessions/{}", ada.session_id),
        )
        .bearer(&phone.access)
        .send()
        .await;
    execute_on(&mut held, "ROLLBACK").await;
    let renamed = rename.await.unwrap();
    let next = api.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(ended.status, 204);
    assert_eq!(renamed.status, 200, "{}", renamed.json());
    assert_eq!(
        db.owner_reads(&format!(
            "SELECT name FROM workspaces WHERE id = '{workspace}'"
        ))
        .await,
        ["Engines"]
    );
    assert_eq!(next.status, 401);
    assert_eq!(next.json()["code"], "request.unauthenticated");
}

#[tokio::test]
async fn auth_010_refreshing_rotates_the_pair_and_keeps_the_old_refresh_hash() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = users::refresh(&api, &ada.refresh).await;

    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["id"], ada.session_id.as_str());
    let access = body["access_token"].as_str().unwrap();
    assert_ne!(access, ada.access);
    let refresh = body["refresh_token"].as_str().unwrap();
    assert_ne!(refresh, ada.refresh);
    let old = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(old.status, 401);
    let new = api.get("/v1/users/me").bearer(access).send().await;
    assert_eq!(new.status, 200);
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT coalesce(refresh_token_hash = sha256(convert_to('{refresh}', 'UTF8')) \
                 AND previous_refresh_token_hash = sha256(convert_to('{}', 'UTF8')), false)::text \
                 FROM sessions",
                ada.refresh
            ))
            .await,
        ["true"],
        "the old refresh hash is kept as the previous one"
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT action FROM audit_entries WHERE action = 'session.refreshed'")
            .await,
        ["session.refreshed"]
    );
}

#[tokio::test]
async fn auth_010_a_refresh_token_that_is_empty_or_longer_than_64_characters_answers_400_at_its_field()
 {
    let api = Api::new().await;
    let (longest, too_long) = ("x".repeat(64), "x".repeat(65));

    let unknown = api
        .post_raw("/v1/sessions/refresh", &json!({ "refresh_token": longest }))
        .send()
        .await;
    for token in ["", too_long.as_str()] {
        let response = api
            .post_raw("/v1/sessions/refresh", &json!({ "refresh_token": token }))
            .send()
            .await;

        assert_eq!(response.status, 400, "{token}: {}", response.json());
        assert_eq!(
            response.json()["errors"],
            json!([{
                "field": "/refresh_token",
                "code": "invalid_refresh_token",
                "message": "The token must be 1 to 64 characters.",
            }])
        );
    }
    assert_eq!(unknown.status, 401);
    assert_eq!(unknown.json()["code"], "auth.invalid_refresh_token");
}

#[tokio::test]
async fn auth_010_a_refresh_slides_the_refresh_expiry() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    db.owner_executes(
        "UPDATE sessions SET access_token_expires_at = now() + interval '1 minute', \
         refresh_token_expires_at = now() + interval '1 minute'",
    )
    .await;

    let response = users::refresh(&api, &ada.refresh).await;

    assert_eq!(response.status, 200);
    assert_eq!(
        db.owner_reads(
            "SELECT (refresh_token_expires_at > now() + interval '29 days')::text FROM sessions"
        )
        .await,
        ["true"]
    );
}

#[tokio::test]
async fn auth_010_a_refresh_through_the_cookie_answers_with_new_cookies() {
    let api = browsers_api().await;
    let signed_in = browser_sign_in(&api).await;
    let (access, refresh) = (
        cookie(&signed_in, "access_token"),
        cookie(&signed_in, "refresh_token"),
    );

    let response = api
        .post("/v1/sessions/refresh", &json!({}))
        .header("origin", WEB)
        .header("cookie", &format!("__Host-refresh_token={refresh}"))
        .send()
        .await;

    assert_eq!(response.status, 200);
    let body = response.json();
    assert!(body.get("access_token").is_none());
    assert!(body.get("refresh_token").is_none());
    assert_ne!(cookie(&response, "access_token"), access);
    assert_ne!(cookie(&response, "refresh_token"), refresh);
}

#[tokio::test]
async fn auth_010_a_refresh_never_slides_the_expiry_past_the_absolute_lifetime() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    api.database()
        .owner_executes(
            "UPDATE sessions SET refresh_token_expires_at = now() + interval '1 hour', \
             expires_at = now() + interval '1 hour'",
        )
        .await;

    let response = users::refresh(&api, &ada.refresh).await;

    let body = response.json();
    assert_eq!(body["refresh_token_expires_at"], body["expires_at"]);
}

#[tokio::test]
async fn auth_011_an_expired_refresh_token_or_session_answers_401_and_rotates_nothing() {
    // A refresh token that expired, with the access token it outlives, and an absolute lifetime
    // that passed, with both tokens' expiries, which never pass it (AUTH-007).
    let tokens = "access_token_expires_at = now() - interval '1 second', \
                  refresh_token_expires_at = now() - interval '1 second'";
    let lifetime = format!("{tokens}, expires_at = now() - interval '1 second'");
    for expired in [tokens, lifetime.as_str()] {
        let api = Api::new().await;
        let ada = users::register(&api, "ada@example.com").await;
        let db = api.database();
        db.owner_executes(&format!("UPDATE sessions SET {expired}"))
            .await;
        let before = db
            .owner_reads("SELECT encode(refresh_token_hash, 'hex') FROM sessions")
            .await;

        let response = users::refresh(&api, &ada.refresh).await;

        assert_eq!(response.status, 401, "{expired}");
        assert_eq!(response.json()["code"], "auth.session_expired");
        let after = db
            .owner_reads("SELECT encode(refresh_token_hash, 'hex') FROM sessions")
            .await;
        assert_eq!(before, after);
    }
}

#[tokio::test]
async fn auth_012_presenting_a_rotated_refresh_token_ends_the_session() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let refreshed = users::refresh(&api, &ada.refresh).await.json();
    let mut signals = api.database().signals().await;

    let reused = users::refresh(&api, &ada.refresh).await;
    let unknown = users::refresh(&api, "unknown").await;

    for response in [&reused, &unknown] {
        assert_eq!(response.status, 401);
        assert_eq!(response.json()["code"], "auth.invalid_refresh_token");
    }
    let current = refreshed["access_token"].as_str().unwrap();
    let after = api.get("/v1/users/me").bearer(current).send().await;
    assert_eq!(after.status, 401);
    let db = api.database();
    assert_eq!(
        db.owner_reads("SELECT count(*)::text FROM sessions").await,
        ["0"]
    );
    assert_eq!(
        db.owner_reads(
            "SELECT resource_id::text FROM audit_entries WHERE action = 'session.reuse_detected'"
        )
        .await,
        std::slice::from_ref(&ada.session_id)
    );
    let received = signals.received().await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["session_id"], ada.session_id.as_str());
}

#[tokio::test]
async fn auth_012_a_reused_token_whose_session_a_deletion_ended_first_records_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let rotated = users::refresh(&api, &ada.refresh).await;
    assert_eq!(rotated.status, 200);
    let db = api.database();
    // A deletion of the account that holds the session's row while the reuse waits for it.
    let mut deletion = db
        .holding(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    let reuse = tokio::spawn({
        let api = api.clone();
        let token = ada.refresh.clone();
        async move {
            let response = users::refresh(&api, &token).await;
            (response.status, response.json())
        }
    });
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, body) = reuse.await.unwrap();

    assert_eq!(status, 401);
    assert_eq!(body["code"], "auth.invalid_refresh_token");
    let recorded = db
        .owner_reads(
            "SELECT count(*)::text FROM audit_entries WHERE action = 'session.reuse_detected'",
        )
        .await;
    assert_eq!(recorded, ["0"]);
}

#[tokio::test]
async fn auth_012_a_refresh_without_a_token_answers_the_same_401() {
    let api = Api::new().await;

    let response = api.post("/v1/sessions/refresh", &json!({})).send().await;

    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "auth.invalid_refresh_token");
}

#[tokio::test]
async fn auth_013_signing_out_ends_the_session_and_clears_its_cookies() {
    // Ending the session that authenticated the request by its ID signs out too (AUTH-044).
    for by_id in [false, true] {
        let api = Api::new().await;
        let ada = users::register(&api, "ada@example.com").await;
        let mut signals = api.database().signals().await;
        let path = if by_id {
            format!("/v1/users/me/sessions/{}", ada.session_id)
        } else {
            "/v1/users/me/sessions/current".to_owned()
        };

        let response = api
            .request(http::Method::DELETE, &path)
            .bearer(&ada.access)
            .send()
            .await;

        assert_eq!(response.status, 204, "{path}");
        let cleared = response.headers("set-cookie");
        assert!(
            cleared.contains(
                &"__Host-access_token=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict"
            ),
            "{path}"
        );
        assert!(
            cleared.contains(
                &"__Host-refresh_token=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict"
            ),
            "{path}"
        );
        let after = api.get("/v1/users/me").bearer(&ada.access).send().await;
        assert_eq!(after.status, 401, "{path}");
        assert_eq!(
            api.database()
                .owner_reads("SELECT count(*)::text FROM sessions")
                .await,
            ["0"],
            "{path}: the session is deleted"
        );
        assert_eq!(
            api.database()
                .owner_reads(
                    "SELECT resource_id::text FROM audit_entries WHERE action = 'session.ended'"
                )
                .await,
            std::slice::from_ref(&ada.session_id),
            "{path}"
        );
        let received: Vec<Value> = signals.received().await;
        assert_eq!(received.len(), 1, "{path}");
        assert_eq!(received[0]["kind"], "session_ended", "{path}");
        assert_eq!(received[0]["user_id"], ada.user_id.as_str(), "{path}");
    }
}

#[tokio::test]
async fn auth_015_a_browser_takes_its_tokens_as_cookies_it_can_t_read() {
    let api = browsers_api().await;

    let response = browser_sign_in(&api).await;

    assert_eq!(response.status, 201);
    let session = response.json();
    assert!(session["id"].is_string());
    assert!(session.get("access_token").is_none());
    assert!(session.get("refresh_token").is_none());
    let set = response.headers("set-cookie");
    assert_eq!(set.len(), 3);
    assert!(session.get("device_token").is_none());
    for cookie in set {
        assert!(
            cookie.starts_with("__Host-access_token=")
                || cookie.starts_with("__Host-refresh_token=")
                || cookie.starts_with("__Host-device_token_")
        );
        assert!(
            cookie.ends_with("; Path=/; Secure; HttpOnly; SameSite=Strict"),
            "{cookie}"
        );
        assert!(cookie.contains("; Expires="), "{cookie}");
        assert!(!cookie.contains("Domain"), "{cookie}");
    }
}

#[tokio::test]
async fn auth_015_a_browser_s_registration_takes_cookies_that_expire_with_their_tokens() {
    let api = browsers_api().await;

    let response = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .header("origin", WEB)
        .send()
        .await;

    assert_eq!(response.status, 201);
    let session = response.json();
    assert!(session.get("access_token").is_none());
    assert!(session.get("refresh_token").is_none());
    // Postgres writes each expiry as HTTP writes a date, to the second.
    let stored = api
        .database()
        .owner_reads(
            "SELECT to_char(access_token_expires_at AT TIME ZONE 'UTC', \
             'Dy, DD Mon YYYY HH24:MI:SS \"GMT\"') || '|' || \
             to_char(refresh_token_expires_at AT TIME ZONE 'UTC', \
             'Dy, DD Mon YYYY HH24:MI:SS \"GMT\"') || '|' || \
             (SELECT to_char(expires_at AT TIME ZONE 'UTC', \
             'Dy, DD Mon YYYY HH24:MI:SS \"GMT\"') FROM device_tokens) FROM sessions",
        )
        .await;
    assert_eq!(
        stored,
        [format!(
            "{}|{}|{}",
            expires(&response, "access_token"),
            expires(&response, "refresh_token"),
            expires(&response, &users::device_cookie(&response))
        )]
    );
}

#[tokio::test]
async fn auth_015_a_browser_s_refresh_takes_cookies_even_with_its_token_in_the_body() {
    let api = browsers_api().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = api
        .post(
            "/v1/sessions/refresh",
            &json!({ "refresh_token": ada.refresh }),
        )
        .header("origin", WEB)
        .send()
        .await;

    assert_eq!(response.status, 200);
    let body = response.json();
    assert!(body.get("access_token").is_none());
    assert!(body.get("refresh_token").is_none());
    assert_ne!(cookie(&response, "refresh_token"), ada.refresh);
}

/// The `Set-Cookie` of the cookie `__Host-<name>` that `response` sets, which a browser's script
/// can't read, and which a browser sends to the API's host alone.
fn unreadable_cookie<'a>(response: &'a Response, name: &str) -> &'a str {
    let prefix = format!("__Host-{name}=");
    let set = response
        .headers("set-cookie")
        .into_iter()
        .find(|set| set.starts_with(&prefix));
    assert!(set.is_some(), "no {name} cookie");
    let set = set.unwrap();
    assert!(
        set.ends_with("; Path=/; Secure; HttpOnly; SameSite=Strict"),
        "{set}"
    );
    assert!(!set.contains("Domain"), "{set}");
    set
}

/// Asserts that `response`, a browser's, holds no token in its body, and sets the access and
/// refresh tokens of the session `session_id` in their cookies, each expiring with its token.
async fn tokens_in_cookies(api: &Api, response: &Response, session_id: &str) {
    let body = response.json();
    assert!(body.get("access_token").is_none(), "{body}");
    assert!(body.get("refresh_token").is_none(), "{body}");
    for name in ["access_token", "refresh_token"] {
        unreadable_cookie(response, name);
    }
    let stored = api
        .database()
        .owner_reads(&format!(
            "SELECT (access_token_hash = sha256(convert_to('{}', 'UTF8')) \
             AND refresh_token_hash = sha256(convert_to('{}', 'UTF8')))::text || '|' || \
             to_char(access_token_expires_at AT TIME ZONE 'UTC', \
             'Dy, DD Mon YYYY HH24:MI:SS \"GMT\"') || '|' || \
             to_char(refresh_token_expires_at AT TIME ZONE 'UTC', \
             'Dy, DD Mon YYYY HH24:MI:SS \"GMT\"') FROM sessions WHERE id = '{session_id}'",
            cookie(response, "access_token"),
            cookie(response, "refresh_token"),
        ))
        .await;
    assert_eq!(
        stored,
        [format!(
            "true|{}|{}",
            expires(response, "access_token"),
            expires(response, "refresh_token")
        )]
    );
}

#[tokio::test]
async fn auth_015_a_browser_s_completion_of_a_sign_in_takes_its_tokens_in_cookies() {
    let api = browsers_api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = second_factors::add_app(&api, &ada).await;
    let challenged = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD }),
        )
        .header("origin", WEB)
        .send()
        .await;
    assert_eq!(challenged.status, 200, "{}", challenged.json());
    let challenge = cookie(&challenged, "sign_in_challenge");

    let response = api
        .post(
            "/v1/sessions/second-factor",
            &json!({ "code": second_factors::code(&api, &app.secret).await }),
        )
        .header("origin", WEB)
        .header("cookie", &format!("__Host-sign_in_challenge={challenge}"))
        .send()
        .await;

    assert_eq!(response.status, 201, "{}", response.json());
    let session = response.json();
    tokens_in_cookies(&api, &response, session["id"].as_str().unwrap()).await;
    assert!(session.get("device_token").is_none(), "{session}");
    let name = users::device_cookie(&response);
    assert_eq!(name.len(), "device_token_".len() + 16, "{name}");
    assert!(
        name["device_token_".len()..]
            .chars()
            .all(|digit| digit.is_ascii_hexdigit()),
        "{name}"
    );
    unreadable_cookie(&response, &name);
    let device_token = api
        .database()
        .owner_reads(&format!(
            "SELECT to_char(expires_at AT TIME ZONE 'UTC', 'Dy, DD Mon YYYY HH24:MI:SS \"GMT\"') \
             FROM device_tokens WHERE token_hash = sha256(convert_to('{}', 'UTF8'))",
            cookie(&response, &name)
        ))
        .await;
    assert_eq!(device_token, [expires(&response, &name)]);
}

#[tokio::test]
async fn auth_015_a_browser_s_proofs_take_the_session_s_new_tokens_in_cookies() {
    let upstream = Upstream::start().await;
    let api = Api::with(providers::stand_in(&upstream).set("CORS_ALLOWED_ORIGINS", WEB)).await;
    let with_password = browser_sign_in(&api).await;
    assert_eq!(with_password.status, 201, "{}", with_password.json());
    gives(&upstream, identity("subject-1", "grace@example.com", true));
    let through_provider = api
        .post("/v1/sessions/provider", &grant())
        .header("origin", WEB)
        .send()
        .await;
    assert_eq!(through_provider.status, 201, "{}", through_provider.json());
    // The cookies that each user's browser holds, which each proof's replace.
    let mut jars = [
        users::cookies(&with_password),
        users::cookies(&through_provider),
    ];
    let sessions =
        [&with_password, &through_provider].map(|signed_in| signed_in.json()["id"].clone());
    let reauthenticate = json!({ "password": PASSWORD });
    let change = json!({ "current_password": PASSWORD, "password": "walnut-harbor-77-brisk" });

    for (path, body, user) in [
        (
            "/v1/users/me/sessions/current/reauthenticate",
            reauthenticate,
            0,
        ),
        ("/v1/users/me/password/change", change, 0),
        (
            "/v1/users/me/sessions/current/reauthenticate/provider",
            grant(),
            1,
        ),
    ] {
        let response = api
            .post(path, &body)
            .header("origin", WEB)
            .header("cookie", &jars[user])
            .send()
            .await;

        assert_eq!(response.status, 200, "{path}: {}", response.json());
        tokens_in_cookies(&api, &response, sessions[user].as_str().unwrap()).await;
        jars[user] = users::cookies(&response);
    }
}

#[tokio::test]
async fn auth_016_a_session_that_another_request_ended_meanwhile_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // The user bucket, which every request of Ada's counts in once it is authenticated.
    let bucket =
        users::added_bucket(&api, api.get("/v1/users/me").bearer(&ada.access).send()).await;

    let (status, body) = users::after_its_session_ended(
        &api,
        &ada,
        &bucket,
        (http::Method::GET, "/v1/users/me/sessions/current"),
        None,
    )
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
}

#[tokio::test]
async fn auth_016_a_read_whose_account_a_deletion_takes_after_its_session_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let bucket =
        users::added_bucket(&api, api.get("/v1/users/me").bearer(&ada.access).send()).await;
    // The read waits at the user bucket once it is authenticated, which read the account.
    let mut limit = db
        .holding(&format!(
            "SELECT 1 FROM rate_limit_buckets WHERE id = '{bucket}' FOR UPDATE"
        ))
        .await;

    let read = api.spawn(
        http::Method::GET,
        "/v1/users/me/sessions/current",
        &ada.access,
        None,
    );
    queries_wait_on_locks(db, 1).await;
    // Then a deletion holds `users`, so the read finds the session and waits at the account.
    let mut deletion = db
        .holding("LOCK TABLE users IN ACCESS EXCLUSIVE MODE")
        .await;
    execute_on(&mut limit, "COMMIT").await;
    loop {
        let waiting = queries_waiting_on_locks(db).await;
        if waiting.iter().any(|query| query.contains("FROM users")) {
            break;
        }
        tokio::task::yield_now().await;
    }
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

#[tokio::test]
async fn auth_016_a_session_that_the_job_deleted_meanwhile_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let bucket =
        users::added_bucket(&api, api.get("/v1/users/me").bearer(&ada.access).send()).await;
    let mut held = db
        .holding(&format!(
            "SELECT 1 FROM rate_limit_buckets WHERE id = '{bucket}' FOR UPDATE"
        ))
        .await;

    let read = tokio::spawn({
        let api = api.clone();
        let access = ada.access.clone();
        async move {
            let response = api
                .get("/v1/users/me/sessions/current")
                .bearer(&access)
                .send()
                .await;
            (response.status, response.json())
        }
    });
    queries_wait_on_locks(db, 1).await;
    // The session stops lasting while the request waits, and the hourly job deletes it.
    users::stopped_lasting(&api, &ada).await;
    api.run_periodic("auth.expire_sessions").await;
    execute_on(&mut held, "COMMIT").await;
    let (status, body) = read.await.unwrap();

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert!(
        db.owner_reads("SELECT id::text FROM sessions")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_016_the_current_session_is_readable_without_its_tokens() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let body = json!({
        "email": "ada@example.com",
        "password": PASSWORD,
        "device_id": "phone_2",
        "device_name": "Ada's phone",
    });
    let signed_in = api.post("/v1/sessions", &body).send().await.json();
    let access = signed_in["access_token"].as_str().unwrap();

    let response = api
        .get("/v1/users/me/sessions/current")
        .bearer(access)
        .send()
        .await;

    assert_eq!(response.status, 200);
    let session = response.json();
    assert_eq!(session["id"], signed_in["id"]);
    assert_eq!(session["user_id"], signed_in["user_id"]);
    assert_eq!(session["device_id"], "phone_2");
    assert_eq!(session["device_name"], "Ada's phone");
    assert_eq!(session.get("provider"), Some(&json!(null)), "a password's");
    assert!(session.get("access_token").is_none());
    assert!(session.get("refresh_token").is_none());
    for field in [
        "created_at",
        "access_token_expires_at",
        "refresh_token_expires_at",
        "expires_at",
        "authenticated_at",
    ] {
        let at = session[field].as_str().unwrap();
        assert!(at.ends_with('Z') && at.len() == 27, "{field}: {at}");
    }
    // A sign-in proves the password, so the session starts as authenticated then.
    assert_eq!(session["authenticated_at"], session["created_at"]);
}
