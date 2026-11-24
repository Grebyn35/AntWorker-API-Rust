//! Registration and sign-in (AUTH-001 to AUTH-008, AUTH-014).

use argon2::{Algorithm, Argon2, Params, PasswordHasher, Version};
use platform::telemetry::testing::collector::Collector;
use serde_json::json;

use crate::support::{
    Api, Options, TestDatabase, answer_before_a_lock_wait,
    api::Response,
    execute_on, limited_while_holding, query_waits_on_a_lock,
    users::{self, PASSWORD, sign_in_from, signing_in},
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
async fn auth_004_a_presented_password_holds_1_to_128_characters() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let presenting = [
        (
            "/v1/sessions",
            json!({ "email": "ada@example.com" }),
            "password",
            None,
            401_u16,
        ),
        (
            "/v1/users/me/sessions/current/reauthenticate",
            json!({}),
            "password",
            Some(&ada.access),
            403,
        ),
        (
            "/v1/users/me/password/change",
            json!({ "password": PASSWORD }),
            "current_password",
            Some(&ada.access),
            403,
        ),
    ];

    for (path, mut body, field, access, checked) in presenting {
        let mut answers = Vec::new();
        for password in [
            String::new(),
            "é".repeat(129),
            "é".to_owned(),
            "é".repeat(128),
        ] {
            body[field] = json!(password);
            let mut call = api.post_raw(path, &body);
            if let Some(access) = access {
                call = call.bearer(access);
            }
            answers.push(call.send().await);
        }

        let pointer = format!("/{field}");
        assert_eq!(
            answers[0].refused_fields(),
            [(pointer.clone(), "required".to_owned())],
            "{path}"
        );
        assert_eq!(
            answers[1].refused_fields(),
            [(pointer, "too_long".to_owned())],
            "{path}"
        );
        assert_eq!(answers[2].status, checked, "{path}");
        assert_eq!(answers[3].status, checked, "{path}");
    }
}

#[tokio::test]
async fn auth_005_every_failed_sign_in_answers_the_same_401() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
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
}

#[tokio::test]
async fn auth_005_a_wrong_password_and_a_password_for_an_account_without_one_record_sign_in_failed_in_its_log()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let without = users::without_password(api.database(), "nopassword@example.com").await;

    users::sign_in(&api, "ada@example.com", "wrong-password-123").await;
    users::sign_in(&api, "nobody@example.com", PASSWORD).await;
    users::sign_in(&api, "nopassword@example.com", PASSWORD).await;

    let failures = api
        .database()
        .owner_reads(&format!(
            "SELECT CASE user_id WHEN '{}' THEN 'ada' WHEN '{without}' THEN 'without' END \
             || ' ' || coalesce(actor_user_id::text, 'none') FROM audit_entries \
             WHERE action = 'sign_in.failed' ORDER BY 1",
            ada.user_id
        ))
        .await;
    assert_eq!(failures, ["ada none", "without none"]);
}

#[tokio::test]
async fn auth_005_an_unknown_email_and_an_account_without_a_password_verify_a_hash_as_a_wrong_password_does()
 {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let endpoint = collector.endpoint();
    let server = database
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &endpoint),
        ])
        .await;
    let registration = users::registration("ada@example.com");
    let registered = server
        .post_in_trace(
            "/v1/users",
            &registration,
            "0af7651916cd43dd8448eb211c80319c",
        )
        .await;
    assert!(registered.starts_with("HTTP/1.1 201"), "{registered}");
    users::without_password(&database, "nopassword@example.com").await;
    let attempts = [
        (
            "ada@example.com",
            "wrong-password-123",
            "5bf1a8b1a1e04e7a8c0f1d2e3a4b5c01",
        ),
        (
            "nobody@example.com",
            PASSWORD,
            "5bf1a8b1a1e04e7a8c0f1d2e3a4b5c02",
        ),
        (
            "nopassword@example.com",
            PASSWORD,
            "5bf1a8b1a1e04e7a8c0f1d2e3a4b5c03",
        ),
    ];

    let mut answers = Vec::new();
    for (email, password, trace) in attempts {
        let body = json!({ "email": email, "password": password });
        answers.push(server.post_in_trace("/v1/sessions", &body, trace).await);
    }

    for answer in &answers {
        assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
    }
    let traces: Vec<&str> = attempts.iter().map(|(_, _, trace)| *trace).collect();
    assert_eq!(
        users::password_verifications(&collector, &traces).await,
        [1, 1, 1]
    );
}

/// The `sign_in.failed` entries of `user`'s log.
async fn failures(api: &Api, user: &users::SignedIn) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT action FROM audit_entries WHERE action = 'sign_in.failed' AND user_id = '{}'",
            user.user_id
        ))
        .await
}

#[tokio::test]
async fn sec_05_a_wrong_password_answers_401_while_another_transaction_holds_the_password_identity()
{
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let mut held = users::holding_identities(db, &ada).await;

    let answer = answer_before_a_lock_wait(
        db,
        users::sign_in(&api, "ada@example.com", "wrong-password-123"),
    )
    .await;

    execute_on(&mut held, "COMMIT").await;
    assert!(
        answer.is_some(),
        "the sign-in waited for the identity's lock"
    );
    let answer = answer.unwrap();
    assert_eq!(answer.status, 401, "{}", answer.json());
    assert_eq!(answer.json()["code"], "auth.invalid_credentials");
}

#[tokio::test]
async fn auth_005_a_password_that_a_change_replaced_once_the_sign_in_read_its_hash_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let sessions = db.owner_reads("SELECT id::text FROM sessions").await;
    let other = Argon2::default()
        .hash_password_with_salt(b"lantern-orchard-58-brisk", b"saltsaltsaltsalt")
        .unwrap()
        .to_string();
    // A change of the password, which holds the identity as it sets the hash.
    let mut changing = users::holding_identities(db, &ada).await;

    let signed_in = signing_in(&api, "ada@example.com");
    // The sign-in read the hash, which its password matched, and waits for the identity's lock.
    query_waits_on_a_lock(db, "FROM identities").await;
    execute_on(
        &mut changing,
        &format!(
            "UPDATE identities SET password_hash = '{other}' WHERE user_id = '{}'",
            ada.user_id
        ),
    )
    .await;
    execute_on(&mut changing, "COMMIT").await;
    let answer = signed_in.await.unwrap();

    assert_eq!(answer.status, 401, "{}", answer.json());
    assert_eq!(answer.json()["code"], "auth.invalid_credentials");
    assert_eq!(failures(&api, &ada).await, ["sign_in.failed"]);
    assert_eq!(
        db.owner_reads("SELECT id::text FROM sessions").await,
        sessions
    );
}

#[tokio::test]
async fn auth_005_a_password_identity_that_went_once_the_sign_in_read_its_hash_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let sessions = db.owner_reads("SELECT id::text FROM sessions").await;
    // A removal of the password, which holds the identity as it deletes it.
    let mut removing = users::holding_identities(db, &ada).await;

    let signed_in = signing_in(&api, "ada@example.com");
    // The sign-in read the hash, which its password matched, and waits for the identity's lock.
    query_waits_on_a_lock(db, "FROM identities").await;
    execute_on(
        &mut removing,
        &format!("DELETE FROM identities WHERE user_id = '{}'", ada.user_id),
    )
    .await;
    execute_on(&mut removing, "COMMIT").await;
    let answer = signed_in.await.unwrap();

    assert_eq!(answer.status, 401, "{}", answer.json());
    assert_eq!(answer.json()["code"], "auth.invalid_credentials");
    assert_eq!(failures(&api, &ada).await, ["sign_in.failed"]);
    assert_eq!(
        db.owner_reads("SELECT id::text FROM sessions").await,
        sessions
    );
}

#[tokio::test]
async fn auth_005_a_wrong_password_whose_account_a_deletion_takes_before_its_record_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    // Held as a deletion of the account holds it, before it deletes the row.
    let mut deleting = db
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    let signed_in = {
        let api = api.clone();
        tokio::spawn(
            async move { users::sign_in(&api, "ada@example.com", "wrong-password-123").await },
        )
    };
    // The sign-in verified the password, and its record waits for the account's row.
    query_waits_on_a_lock(db, "INSERT INTO audit_entries").await;
    execute_on(
        &mut deleting,
        &format!("DELETE FROM users WHERE id = '{}'", ada.user_id),
    )
    .await;
    execute_on(&mut deleting, "COMMIT").await;
    let answer = signed_in.await.unwrap();

    assert_eq!(answer.status, 401, "{}", answer.json());
    assert_eq!(answer.json()["code"], "auth.invalid_credentials");
}

#[tokio::test]
async fn auth_006_each_email_allows_its_attempts_from_each_address_then_answers_429_before_any_lookup()
 {
    let api = Api::with(Options::default().set("SIGN_IN_ATTEMPTS", 2)).await;

    let first = sign_in_from(
        &api,
        "[2001:db8:1:2::1]:50000",
        "nobody@example.com",
        PASSWORD,
        None,
    )
    .await;
    let second = sign_in_from(
        &api,
        "[2001:db8:1:2::2]:50000",
        "NOBODY@example.com",
        PASSWORD,
        None,
    )
    .await;
    let padded = json!({ "email": " nobody@example.com", "password": PASSWORD });
    let third = api
        .post_raw("/v1/sessions", &padded)
        .from("[2001:db8:1:2:ffff::9]:50000")
        .send()
        .await;
    let other_email = sign_in_from(
        &api,
        "[2001:db8:1:2::1]:50000",
        "someone@example.com",
        PASSWORD,
        None,
    )
    .await;
    let other_address = sign_in_from(
        &api,
        "[2001:db8:1:3::1]:50000",
        "nobody@example.com",
        PASSWORD,
        None,
    )
    .await;

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
        .set("SIGN_IN_ATTEMPTS", 1)
        .set("SIGN_IN_EMAIL_ATTEMPTS", 2);
    let api = Api::with(options).await;
    users::register(&api, "ada@example.com").await;

    // The second from one address is that address's refusal, which the email's count leaves out.
    let mut one = Vec::new();
    for _ in 0..2 {
        one.push(
            sign_in_from(&api, "192.0.2.1:50000", "ada@example.com", PASSWORD, None)
                .await
                .status,
        );
    }
    let another = sign_in_from(&api, "192.0.2.2:50000", "ada@example.com", PASSWORD, None).await;
    let third = sign_in_from(&api, "192.0.2.3:50000", "ada@example.com", PASSWORD, None).await;
    let other_email =
        sign_in_from(&api, "192.0.2.3:50000", "grace@example.com", PASSWORD, None).await;

    assert_eq!(one, [201, 429]);
    assert_eq!(another.status, 201);
    assert_eq!(third.status, 429);
    assert_eq!(third.json()["code"], "request.rate_limited");
    assert!(
        third
            .header("ratelimit-policy")
            .starts_with("\"sign_in_email\";q=2;w=900")
    );
    assert_eq!(other_email.status, 401);
}

#[tokio::test]
async fn auth_006_by_default_an_email_allows_5_attempts_from_an_address_and_50_from_all_together() {
    // An empty setting counts as unset (OPS-01), so each API reads the default of one of the two
    // limits, which the test support otherwise raises. Beside the default from every address,
    // the limit from one address is 25, the most that startup takes with it (AUTH-006).
    let address = Api::with(Options::default().set("SIGN_IN_ATTEMPTS", "")).await;
    let every = Api::with(
        Options::default()
            .set("SIGN_IN_EMAIL_ATTEMPTS", "")
            .set("SIGN_IN_ATTEMPTS", 25),
    )
    .await;

    let from_address = sign_in_from(
        &address,
        "192.0.2.1:50000",
        "nobody@example.com",
        PASSWORD,
        None,
    )
    .await;
    // After an attempt from each of 26 addresses, one from a 27th leaves the count of every
    // address 23 attempts, fewer than the 24 that the count of its address leaves.
    for host in 1..=26 {
        sign_in_from(
            &every,
            &format!("192.0.2.{host}:50000"),
            "nobody@example.com",
            PASSWORD,
            None,
        )
        .await;
    }
    let from_every = sign_in_from(
        &every,
        "192.0.2.27:50000",
        "nobody@example.com",
        PASSWORD,
        None,
    )
    .await;

    // Each answer names the bucket with the fewest attempts left (PLATFORM-056).
    assert_eq!(from_address.status, 401);
    assert!(
        from_address
            .header("ratelimit-policy")
            .starts_with("\"sign_in\";q=5;w=900"),
        "{}",
        from_address.header("ratelimit-policy")
    );
    assert_eq!(from_every.status, 401);
    assert!(
        from_every
            .header("ratelimit-policy")
            .starts_with("\"sign_in_email\";q=50;w=900"),
        "{}",
        from_every.header("ratelimit-policy")
    );
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
async fn auth_006_a_sign_in_beyond_its_limit_answers_429_before_it_reads_the_account() {
    let api = Api::with(Options::default().set("SIGN_IN_ATTEMPTS", 1)).await;
    users::register(&api, "ada@example.com").await;
    let first = sign_in_from(&api, "192.0.2.1:50000", "ada@example.com", PASSWORD, None).await;
    assert_eq!(first.status, 201, "{}", first.json());

    // Held against every read of an account.
    limited_while_holding(
        api.database(),
        "LOCK TABLE users IN ACCESS EXCLUSIVE MODE",
        sign_in_from(&api, "192.0.2.1:50000", "ada@example.com", PASSWORD, None),
    )
    .await;
}

#[tokio::test]
async fn auth_007_a_session_holds_two_random_tokens_stored_only_as_hashes() {
    let api = Api::new().await;
    let registered = users::register(&api, "ada@example.com").await;
    let signed_in = users::new_session(&api, "ada@example.com").await;

    for (route, ada) in [("registration", registered), ("sign-in", signed_in)] {
        assert_eq!(ada.access.len(), 43, "{route}");
        assert_eq!(ada.refresh.len(), 43, "{route}");
        assert_ne!(ada.access, ada.refresh, "{route}");
        let session = &ada.session_id;
        let stored = api
            .database()
            .owner_reads(&format!(
                "SELECT encode(access_token_hash, 'base64') || encode(refresh_token_hash, 'base64') \
                 FROM sessions WHERE id = '{session}'"
            ))
            .await;
        assert!(
            !stored[0].contains(&ada.access) && !stored[0].contains(&ada.refresh),
            "{route}"
        );
        let hashed = api
            .database()
            .owner_reads(&format!(
                "SELECT (access_token_hash = sha256(convert_to('{}', 'UTF8')) \
                 AND refresh_token_hash = sha256(convert_to('{}', 'UTF8')))::text \
                 FROM sessions WHERE id = '{session}'",
                ada.access, ada.refresh
            ))
            .await;
        assert_eq!(hashed, ["true"], "{route}: SHA-256 hashes");
        let lifetimes = api
            .database()
            .owner_reads(&format!(
                "SELECT round(extract(epoch FROM access_token_expires_at - created_at))::text \
                 || ' ' || round(extract(epoch FROM refresh_token_expires_at - created_at))::text \
                 || ' ' || round(extract(epoch FROM expires_at - created_at))::text \
                 FROM sessions WHERE id = '{session}'"
            ))
            .await;
        assert_eq!(lifetimes, ["3600 2592000 7776000"], "{route}");
    }
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
async fn auth_007_no_token_outlives_its_session_s_absolute_lifetime() {
    // The absolute lifetime is the shortest of the three, so it caps both tokens.
    let options = Options::default()
        .set("ACCESS_TOKEN_SECONDS", 7_200)
        .set("REFRESH_TOKEN_SECONDS", 7_200)
        .set("SESSION_LIFETIME_SECONDS", 3_600);
    let api = Api::with(options).await;
    let ada = users::register(&api, "ada@example.com").await;
    let lifetimes = "SELECT round(extract(epoch FROM access_token_expires_at - created_at))::text \
                     || ' ' || round(extract(epoch FROM refresh_token_expires_at - created_at))::text \
                     || ' ' || round(extract(epoch FROM expires_at - created_at))::text FROM sessions";
    let started = api.database().owner_reads(lifetimes).await;

    let refreshed = users::refresh(&api, &ada.refresh).await;

    assert_eq!(started, ["3600 3600 3600"]);
    assert_eq!(refreshed.status, 200);
    let session = refreshed.json();
    assert_eq!(session["access_token_expires_at"], session["expires_at"]);
    assert_eq!(session["refresh_token_expires_at"], session["expires_at"]);
}

#[tokio::test]
async fn auth_007_a_check_refuses_a_session_whose_expiries_are_out_of_their_order() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let db = api.database();

    let access = db
        .try_execute(
            &db.owner_url(),
            "UPDATE sessions SET access_token_expires_at = \
             refresh_token_expires_at + interval '1 second'",
        )
        .await;
    let refresh = db
        .try_execute(
            &db.owner_url(),
            "UPDATE sessions SET refresh_token_expires_at = expires_at + interval '1 second'",
        )
        .await;
    let ordered = db
        .try_execute(
            &db.owner_url(),
            "UPDATE sessions SET access_token_expires_at = expires_at, \
             refresh_token_expires_at = expires_at",
        )
        .await;

    for refused in [access, refresh] {
        let message = refused.unwrap_err();
        assert!(message.contains("sessions_expires_at_check"), "{message}");
    }
    assert_eq!(ordered, Ok(()));
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
async fn auth_008_a_sign_in_on_a_device_ends_its_own_user_s_session_there_and_no_other_user_s() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    users::register(&api, "bob@example.com").await;
    let on_device =
        |email: &str| json!({ "email": email, "password": PASSWORD, "device_id": "shared-1" });
    let ada = api
        .post("/v1/sessions", &on_device("ada@example.com"))
        .send()
        .await;
    assert_eq!(ada.status, 201, "{}", ada.json());
    let bob = api
        .post("/v1/sessions", &on_device("bob@example.com"))
        .send()
        .await;
    assert_eq!(bob.status, 201, "{}", bob.json());

    let again = api
        .post("/v1/sessions", &on_device("bob@example.com"))
        .send()
        .await;

    assert_eq!(again.status, 201, "{}", again.json());
    let id = |response: &Response| response.json()["id"].as_str().unwrap().to_owned();
    let mut on_the_device = vec![id(&ada), id(&again)];
    on_the_device.sort();
    let db = api.database();
    assert_eq!(
        db.owner_reads("SELECT id::text FROM sessions WHERE device_id = 'shared-1' ORDER BY 1")
            .await,
        on_the_device
    );
    assert_eq!(
        db.owner_reads(
            "SELECT resource_id::text FROM audit_entries WHERE action = 'session.ended'"
        )
        .await,
        [id(&bob)]
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
async fn auth_017_a_device_id_is_1_to_64_ascii_letters_digits_underscores_and_hyphens() {
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
                "message": "A device ID has 1 to 64 ASCII letters, digits, underscores and hyphens.",
            }])
        );
    }
}

#[tokio::test]
async fn platform_008_a_failed_sign_in_commits_its_record_then_answers_401() {
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
