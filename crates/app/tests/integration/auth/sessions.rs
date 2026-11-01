//! Authenticated requests, refresh, cookies and signing out (AUTH-009 to AUTH-013, AUTH-015,
//! AUTH-016).

use serde_json::{Value, json};

use crate::support::{
    Api,
    users::{self, PASSWORD, WEB, browser_sign_in, browsers_api, cookie},
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
        .owner_executes("UPDATE sessions SET access_expires_at = now() - interval '1 second'")
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
async fn auth_010_refreshing_rotates_the_pair_and_keeps_the_old_refresh_hash() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = api
        .post(
            "/v1/sessions/refresh",
            &json!({ "refresh_token": ada.refresh }),
        )
        .send()
        .await;

    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["id"], ada.session_id.as_str());
    let access = body["access_token"].as_str().unwrap();
    assert_ne!(access, ada.access);
    assert_ne!(body["refresh_token"].as_str().unwrap(), ada.refresh);
    let old = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(old.status, 401);
    let new = api.get("/v1/users/me").bearer(access).send().await;
    assert_eq!(new.status, 200);
    assert_eq!(
        api.database()
            .owner_reads("SELECT action FROM audit_entries WHERE action = 'session.refreshed'")
            .await,
        ["session.refreshed"]
    );
}

#[tokio::test]
async fn auth_010_a_refresh_slides_the_refresh_expiry() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    db.owner_executes("UPDATE sessions SET refresh_expires_at = now() + interval '1 minute'")
        .await;

    let response = api
        .post(
            "/v1/sessions/refresh",
            &json!({ "refresh_token": ada.refresh }),
        )
        .send()
        .await;

    assert_eq!(response.status, 200);
    assert_eq!(
        db.owner_reads(
            "SELECT (refresh_expires_at > now() + interval '29 days')::text FROM sessions"
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
        .owner_executes("UPDATE sessions SET absolute_expires_at = now() + interval '1 hour'")
        .await;

    let response = api
        .post(
            "/v1/sessions/refresh",
            &json!({ "refresh_token": ada.refresh }),
        )
        .send()
        .await;

    let body = response.json();
    assert_eq!(body["refresh_token_expires_at"], body["expires_at"]);
}

#[tokio::test]
async fn auth_011_an_expired_refresh_token_or_session_answers_401_and_rotates_nothing() {
    for expired in ["refresh_expires_at", "absolute_expires_at"] {
        let api = Api::new().await;
        let ada = users::register(&api, "ada@example.com").await;
        let db = api.database();
        db.owner_executes(&format!(
            "UPDATE sessions SET {expired} = now() - interval '1 second'"
        ))
        .await;
        let before = db
            .owner_reads("SELECT encode(refresh_token_hash, 'hex') FROM sessions")
            .await;

        let response = api
            .post(
                "/v1/sessions/refresh",
                &json!({ "refresh_token": ada.refresh }),
            )
            .send()
            .await;

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
    let refreshed = api
        .post(
            "/v1/sessions/refresh",
            &json!({ "refresh_token": ada.refresh }),
        )
        .send()
        .await
        .json();
    let mut signals = api.database().signals().await;

    let reused = api
        .post(
            "/v1/sessions/refresh",
            &json!({ "refresh_token": ada.refresh }),
        )
        .send()
        .await;
    let unknown = api
        .post(
            "/v1/sessions/refresh",
            &json!({ "refresh_token": "unknown" }),
        )
        .send()
        .await;

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
async fn auth_012_a_refresh_without_a_token_answers_the_same_401() {
    let api = Api::new().await;

    let response = api.post("/v1/sessions/refresh", &json!({})).send().await;

    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "auth.invalid_refresh_token");
}

#[tokio::test]
async fn auth_013_signing_out_ends_the_session_and_clears_its_cookies() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut signals = api.database().signals().await;

    let response = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 204);
    let cleared = response.headers("set-cookie");
    assert!(
        cleared.contains(
            &"__Host-access_token=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict"
        )
    );
    assert!(
        cleared.contains(
            &"__Host-refresh_token=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict"
        )
    );
    let after = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(after.status, 401);
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT resource_id::text FROM audit_entries WHERE action = 'session.ended'"
            )
            .await,
        std::slice::from_ref(&ada.session_id)
    );
    let received: Vec<Value> = signals.received().await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["kind"], "session_ended");
    assert_eq!(received[0]["user_id"], ada.user_id.as_str());
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
    assert_eq!(set.len(), 2);
    for cookie in set {
        assert!(
            cookie.starts_with("__Host-access_token=")
                || cookie.starts_with("__Host-refresh_token=")
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
async fn auth_016_the_current_session_is_readable_without_its_tokens() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let body = json!({ "email": "ada@example.com", "password": PASSWORD, "device_id": "phone_2" });
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
    assert!(session.get("access_token").is_none());
    assert!(session.get("refresh_token").is_none());
    for field in [
        "created_at",
        "access_token_expires_at",
        "refresh_token_expires_at",
        "expires_at",
    ] {
        let at = session[field].as_str().unwrap();
        assert!(at.ends_with('Z') && at.len() == 27, "{field}: {at}");
    }
}
