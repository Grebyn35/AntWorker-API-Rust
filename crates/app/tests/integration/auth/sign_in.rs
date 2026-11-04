//! Registration and sign-in (AUTH-001 to AUTH-008, AUTH-014).

use argon2::{Algorithm, Argon2, Params, PasswordHasher, Version};
use serde_json::json;

use crate::support::{
    Api, Options,
    users::{self, PASSWORD},
};

#[tokio::test]
async fn auth_001_registering_creates_the_user_their_password_a_session_and_a_device_token() {
    let api = Api::new().await;

    let response = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;

    assert_eq!(response.status, 201);
    assert_eq!(response.header("location"), "/v1/users/me/sessions/current");
    let body = response.json();
    let user_id = body["user_id"].as_str().unwrap();
    let session_id = body["id"].as_str().unwrap();
    let db = api.database();
    assert_eq!(
        db.owner_reads("SELECT id::text || ' ' || email FROM users")
            .await,
        [format!("{user_id} ada@example.com")]
    );
    assert_eq!(
        db.owner_reads("SELECT method::text FROM identities").await,
        ["password"]
    );
    assert_eq!(
        db.owner_reads("SELECT user_id::text || ' ' || id::text FROM sessions")
            .await,
        [format!("{user_id} {session_id}")]
    );
    let device_token = body["device_token"].as_str().unwrap();
    assert_eq!(
        db.owner_reads(&format!(
            "SELECT user_id::text FROM device_tokens \
             WHERE token_hash = sha256(convert_to('{device_token}', 'UTF8'))"
        ))
        .await,
        [user_id]
    );
    assert_eq!(
        db.owner_reads(
            "SELECT action || ' ' || scope || ' ' || user_id || ' ' || actor_user_id \
             FROM audit_entries"
        )
        .await,
        [format!("user.registered user {user_id} {user_id}")]
    );
}

#[tokio::test]
async fn auth_002_registering_an_email_in_use_answers_409_and_writes_nothing() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;

    let response = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;

    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "auth.email_in_use");
    let db = api.database();
    for table in [
        "users",
        "identities",
        "sessions",
        "device_tokens",
        "audit_entries",
    ] {
        assert_eq!(
            db.owner_reads(&format!("SELECT count(*)::text FROM {table}"))
                .await,
            ["1"],
            "{table}"
        );
    }
}

#[tokio::test]
async fn auth_003_a_password_the_policy_refuses_is_named_with_its_reason() {
    let api = Api::new().await;
    for (password, reason) in [
        ("short", "too_short"),
        ("password1", "breached"),
        ("AntWorker-rocks-9", "context_word"),
        ("QqQqQqQqQqQqQq", "repeated_character"),
        ("x12345678x", "digit_run"),
    ] {
        let body = json!({ "email": "ada@example.com", "password": password, "name": "Ada" });

        let response = api.post_raw("/v1/users", &body).send().await;

        assert_eq!(response.status, 400, "{password}");
        assert_eq!(response.json()["errors"][0]["field"], "/password");
        assert_eq!(response.json()["errors"][0]["code"], reason, "{password}");
    }
}

#[tokio::test]
async fn auth_004_signing_in_creates_a_session() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = users::sign_in(&api, "ada@example.com", PASSWORD).await;

    assert_eq!(response.status, 201);
    assert_eq!(response.header("location"), "/v1/users/me/sessions/current");
    let body = response.json();
    assert_eq!(body["user_id"], ada.user_id.as_str());
    let session_id = body["id"].as_str().unwrap();
    assert_ne!(session_id, ada.session_id);
    let created = api
        .database()
        .owner_reads("SELECT resource_id::text FROM audit_entries WHERE action = 'session.created'")
        .await;
    assert_eq!(created, [session_id]);
}

#[tokio::test]
async fn auth_005_every_failed_sign_in_answers_the_same_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::without_password(api.database(), "nopassword@example.com").await;

    let wrong = users::sign_in(&api, "ada@example.com", "wrong-password-123").await;
    let unknown = users::sign_in(&api, "nobody@example.com", PASSWORD).await;
    let without = users::sign_in(&api, "nopassword@example.com", PASSWORD).await;

    for response in [&wrong, &unknown, &without] {
        assert_eq!(response.status, 401);
        let mut body = response.json();
        body["trace_id"] = json!(null);
        assert_eq!(
            body,
            json!({
                "type": "/problems/auth.invalid_credentials",
                "title": "Invalid email or password",
                "status": 401,
                "code": "auth.invalid_credentials",
                "trace_id": null,
            })
        );
    }
    let failures = api
        .database()
        .owner_reads(
            "SELECT user_id || ' ' || coalesce(actor_user_id::text, 'none') FROM audit_entries \
             WHERE action = 'sign_in.failed'",
        )
        .await;
    assert_eq!(failures, [format!("{} none", ada.user_id)]);
}

/// Signs `email` in with [`PASSWORD`] from the client address `peer`.
async fn sign_in_from(api: &Api, peer: &str, email: &str) -> crate::support::api::Response {
    api.post(
        "/v1/sessions",
        &json!({ "email": email, "password": PASSWORD }),
    )
    .from(peer)
    .send()
    .await
}

#[tokio::test]
async fn auth_006_each_email_allows_its_attempts_from_each_address_then_answers_429_before_any_lookup()
 {
    let api = Api::with(Options::default().set("SIGN_IN_ATTEMPTS", 2)).await;

    let first = sign_in_from(&api, "[2001:db8:1:2::1]:50000", "nobody@example.com").await;
    let second = sign_in_from(&api, "[2001:db8:1:2::2]:50000", "NOBODY@example.com").await;
    let padded = json!({ "email": " nobody@example.com", "password": PASSWORD });
    let third = api
        .post_raw("/v1/sessions", &padded)
        .from("[2001:db8:1:2:ffff::9]:50000")
        .send()
        .await;
    let other_email = sign_in_from(&api, "[2001:db8:1:2::1]:50000", "someone@example.com").await;
    let other_address = sign_in_from(&api, "[2001:db8:1:3::1]:50000", "nobody@example.com").await;

    assert_eq!(first.status, 401);
    assert_eq!(second.status, 401);
    assert_eq!(third.status, 429, "the same /64");
    assert_eq!(third.json()["code"], "request.rate_limited");
    assert!(
        third
            .header("ratelimit-policy")
            .starts_with("\"sign_in\";q=2;w=900")
    );
    assert_eq!(other_email.status, 401);
    assert_eq!(other_address.status, 401);
}

#[tokio::test]
async fn auth_006_an_email_allows_its_attempts_from_every_address_together_counting_only_those_each_address_lets_through()
 {
    let options = Options::default()
        .set("SIGN_IN_ATTEMPTS", 2)
        .set("SIGN_IN_EMAIL_ATTEMPTS", 3);
    let api = Api::with(options).await;
    users::register(&api, "ada@example.com").await;

    // The third from one address is that address's refusal, which the email's count leaves out.
    let mut one = Vec::new();
    for _ in 0..3 {
        one.push(
            sign_in_from(&api, "192.0.2.1:50000", "ada@example.com")
                .await
                .status,
        );
    }
    let another = sign_in_from(&api, "192.0.2.2:50000", "ada@example.com").await;
    let third = sign_in_from(&api, "192.0.2.3:50000", "ada@example.com").await;
    let other_email = sign_in_from(&api, "192.0.2.3:50000", "grace@example.com").await;

    assert_eq!(one, [201, 201, 429]);
    assert_eq!(another.status, 201);
    assert_eq!(third.status, 429);
    assert_eq!(third.json()["code"], "request.rate_limited");
    assert!(
        third
            .header("ratelimit-policy")
            .starts_with("\"sign_in_email\";q=3;w=900")
    );
    assert_eq!(other_email.status, 401);
}

#[tokio::test]
async fn auth_006_an_existing_email_s_attempts_count_too_in_a_window_that_is_a_setting() {
    let options = Options::default()
        .set("SIGN_IN_ATTEMPTS", 2)
        .set("SIGN_IN_WINDOW_SECONDS", 60);
    let api = Api::with(options).await;
    users::register(&api, "ada@example.com").await;

    let wrong = users::sign_in(&api, "ada@example.com", "wrong-password-123").await;
    let right = users::sign_in(&api, "ada@example.com", PASSWORD).await;
    let refused = users::sign_in(&api, "ada@example.com", PASSWORD).await;

    assert_eq!(wrong.status, 401);
    assert_eq!(right.status, 201);
    assert_eq!(refused.status, 429);
    assert!(
        refused
            .header("ratelimit-policy")
            .starts_with("\"sign_in\";q=2;w=60")
    );
}

#[tokio::test]
async fn auth_007_a_session_holds_two_random_tokens_stored_only_as_hashes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    assert_eq!(ada.access.len(), 43);
    assert_eq!(ada.refresh.len(), 43);
    assert_ne!(ada.access, ada.refresh);
    let stored = api
        .database()
        .owner_reads(
            "SELECT encode(access_token_hash, 'base64') || encode(refresh_token_hash, 'base64') \
             FROM sessions",
        )
        .await;
    assert!(!stored[0].contains(&ada.access) && !stored[0].contains(&ada.refresh));
    let lifetimes = api
        .database()
        .owner_reads(
            "SELECT round(extract(epoch FROM access_token_expires_at - created_at))::text || ' ' || \
             round(extract(epoch FROM refresh_token_expires_at - created_at))::text || ' ' || \
             round(extract(epoch FROM expires_at - created_at))::text FROM sessions",
        )
        .await;
    assert_eq!(lifetimes, ["3600 2592000 7776000"]);
}

#[tokio::test]
async fn auth_007_the_three_lifetimes_are_settings() {
    let options = Options::default()
        .set("ACCESS_TOKEN_SECONDS", 120)
        .set("REFRESH_TOKEN_SECONDS", 7_200)
        .set("SESSION_LIFETIME_SECONDS", 86_400);
    let api = Api::with(options).await;
    users::register(&api, "ada@example.com").await;

    let lifetimes = api
        .database()
        .owner_reads(
            "SELECT round(extract(epoch FROM access_token_expires_at - created_at))::text || ' ' || \
             round(extract(epoch FROM refresh_token_expires_at - created_at))::text || ' ' || \
             round(extract(epoch FROM expires_at - created_at))::text FROM sessions",
        )
        .await;
    assert_eq!(lifetimes, ["120 7200 86400"]);
}

#[tokio::test]
async fn auth_008_signing_in_again_on_a_device_ends_its_old_session() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let body = json!({ "email": "ada@example.com", "password": PASSWORD, "device_id": "laptop-1" });
    let old = api.post("/v1/sessions", &body).send().await.json();
    let mut signals = api.database().signals().await;

    let new = api.post("/v1/sessions", &body).send().await;

    assert_eq!(new.status, 201);
    let old_token = old["access_token"].as_str().unwrap();
    let refused = api.get("/v1/users/me").bearer(old_token).send().await;
    assert_eq!(refused.status, 401);
    let old_id = old["id"].as_str().unwrap();
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT resource_id::text FROM audit_entries WHERE action = 'session.ended'"
            )
            .await,
        [old_id]
    );
    let received = signals.received().await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["kind"], "session_ended");
    assert_eq!(received[0]["session_id"], old_id);
}

#[tokio::test]
async fn auth_008_concurrent_sign_ins_on_one_device_leave_it_one_session() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let body = json!({ "email": "ada@example.com", "password": PASSWORD, "device_id": "laptop-1" });

    let (first, second) = tokio::join!(
        api.post("/v1/sessions", &body).send(),
        api.post("/v1/sessions", &body).send(),
    );

    assert_eq!(first.status, 201);
    assert_eq!(second.status, 201);
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM sessions WHERE device_id = 'laptop-1'")
            .await,
        ["1"]
    );
}

#[tokio::test]
async fn auth_008_without_a_device_id_each_sign_in_creates_a_session() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let second = users::new_session(&api, "ada@example.com").await;

    for token in [&ada.access, &second.access] {
        let response = api.get("/v1/users/me").bearer(token).send().await;
        assert_eq!(response.status, 200);
    }
}

#[tokio::test]
async fn auth_014_passwords_are_argon2id_hashes_at_the_owasp_minimum() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;

    let stored = api
        .database()
        .owner_reads("SELECT password_hash FROM identities")
        .await;
    assert!(
        stored[0].starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
        "{}",
        stored[0]
    );
}

#[tokio::test]
async fn auth_014_a_sign_in_rehashes_a_password_stored_with_other_parameters() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let older = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(8_192, 3, 1, None).unwrap(),
    )
    .hash_password_with_salt(PASSWORD.as_bytes(), b"saltsaltsaltsalt")
    .unwrap()
    .to_string();
    api.database()
        .owner_executes(&format!("UPDATE identities SET password_hash = '{older}'"))
        .await;

    let ada = users::new_session(&api, "ada@example.com").await;

    let db = api.database();
    let stored = db.owner_reads("SELECT password_hash FROM identities").await;
    assert!(stored[0].starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    assert_eq!(
        db.owner_reads(
            "SELECT e.user_id || ' ' || e.actor_user_id || ' ' || e.resource_type || ' ' || \
             (e.resource_id = i.id)::text \
             FROM audit_entries e, identities i WHERE e.action = 'password.rehashed'"
        )
        .await,
        [format!("{0} {0} identity true", ada.user_id)]
    );
}

#[tokio::test]
async fn auth_017_a_device_id_is_1_to_64_letters_digits_underscores_and_hyphens() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let (longest, too_long) = ("x".repeat(64), "x".repeat(65));
    for device_id in ["a", "Laptop_2-b", longest.as_str()] {
        let body =
            json!({ "email": "ada@example.com", "password": PASSWORD, "device_id": device_id });

        let response = api.post("/v1/sessions", &body).send().await;

        assert_eq!(response.status, 201, "{device_id}");
        assert_eq!(response.json()["device_id"], device_id);
    }
    for device_id in ["", "bad id", "caf\u{e9}", "laptop.1", too_long.as_str()] {
        let body =
            json!({ "email": "ada@example.com", "password": PASSWORD, "device_id": device_id });

        let response = api.post_raw("/v1/sessions", &body).send().await;

        assert_eq!(response.status, 400, "{device_id}");
        assert_eq!(
            response.json()["errors"],
            json!([{
                "field": "/device_id",
                "code": "invalid_device_id",
                "message": "A device ID has 1 to 64 letters, digits, underscores and hyphens.",
            }])
        );
    }
}

#[tokio::test]
async fn platform_006_a_failed_sign_in_commits_its_record_then_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = users::sign_in(&api, "ada@example.com", "wrong-password-123").await;

    assert_eq!(response.status, 401);
    let recorded = api
        .database()
        .owner_reads("SELECT user_id::text FROM audit_entries WHERE action = 'sign_in.failed'")
        .await;
    assert_eq!(recorded, [ada.user_id]);
}
