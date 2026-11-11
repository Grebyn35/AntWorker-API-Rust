//! Signing in to an account with an authenticator app: its challenge, and the completion that a
//! code gives; their limits; the jobs of second factors; and the encryption of apps' secrets (the
//! auth spec's "Second factors").

use serde_json::json;

use crate::support::{
    Api, Options, TestDatabase, mailbox,
    second_factors::{add_app, challenge, code, complete, forget_steps, other_code},
    users::{self, PASSWORD, WEB},
};

/// The first column of each row of `sql`, as the `owner` reads it.
async fn reads(api: &Api, sql: &str) -> Vec<String> {
    api.database().owner_reads(sql).await
}

/// The base64 of 32 bytes of `byte`, a key of the key ring.
fn key(byte: u8) -> String {
    data_encoding::BASE64.encode(&[byte; 32])
}

#[tokio::test]
async fn auth_058_signing_in_to_an_account_with_an_app_answers_a_challenge_and_no_session() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    let sessions = reads(&api, "SELECT count(*)::text FROM sessions").await;
    let tokens = reads(&api, "SELECT count(*)::text FROM device_tokens").await;

    let response = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD, "device_id": "laptop",
                     "device_name": "Ada's laptop" }),
        )
        .send()
        .await;

    assert_eq!(response.status, 200);
    let body = response.json();
    let token = body["challenge"].as_str().unwrap();
    assert_eq!(token.len(), 43);
    assert!(body.get("access_token").is_none());
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM sessions").await,
        sessions
    );
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM device_tokens").await,
        tokens
    );
    let stored = reads(
        &api,
        "SELECT (expires_at - created_at)::text || ' ' || device_id || ' ' || device_name || ' ' \
         || (device_token_hash IS NULL)::text FROM sign_in_challenges",
    )
    .await;
    assert_eq!(stored, ["00:05:00 laptop Ada's laptop true"]);
    let hash = reads(
        &api,
        "SELECT encode(token_hash, 'hex') FROM sign_in_challenges",
    )
    .await;
    assert_eq!(hash[0], hex_sha256(token));
    assert_eq!(
        body["expires_at"].as_str().unwrap().len(),
        "2026-11-11T12:00:00.000000Z".len()
    );
    let ada_id = &ada.user_id;
    assert_eq!(
        reads(
            &api,
            "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') FROM audit_entries \
             WHERE action = 'sign_in.challenged'"
        )
        .await,
        [format!("{ada_id} -")]
    );
}

/// The SHA-256 of `token`, in hexadecimal.
fn hex_sha256(token: &str) -> String {
    platform::hashing::sha256(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn auth_058_a_browser_takes_the_challenge_in_a_cookie_and_completes_with_it() {
    let api = users::browsers_api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;

    let response = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD }),
        )
        .header("origin", WEB)
        .send()
        .await;

    assert_eq!(response.status, 200);
    assert!(response.json().get("challenge").is_none());
    let cookie = users::cookie(&response, "sign_in_challenge");
    let completed = api
        .post(
            "/v1/sessions/second-factor",
            &json!({ "code": code(&api, &app.secret).await }),
        )
        .header("origin", WEB)
        .header("cookie", &format!("__Host-sign_in_challenge={cookie}"))
        .send()
        .await;
    assert_eq!(completed.status, 201, "{}", completed.json());
    assert!(completed.json().get("access_token").is_none());
    let cleared = completed
        .headers("set-cookie")
        .into_iter()
        .any(|set| set.starts_with("__Host-sign_in_challenge=;") && set.contains("Max-Age=0"));
    assert!(cleared, "{:?}", completed.headers("set-cookie"));
}

#[tokio::test]
async fn auth_059_a_code_completes_the_sign_in_with_a_session_on_its_device_and_a_device_token() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let first = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD, "device_id": "laptop" }),
        )
        .send()
        .await
        .json();
    let completed = complete(
        &api,
        first["challenge"].as_str().unwrap(),
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;

    assert_eq!(completed.status, 201, "{}", completed.json());
    assert_eq!(
        completed.header("location"),
        "/v1/users/me/sessions/current"
    );
    let session = completed.json();
    assert_eq!(session["device_id"], "laptop");
    assert!(session["access_token"].is_string());
    assert!(session["device_token"].is_string());
    assert!(
        reads(&api, "SELECT id::text FROM sign_in_challenges")
            .await
            .is_empty()
    );
    let created = reads(
        &api,
        &format!(
            "SELECT count(*)::text FROM audit_entries WHERE action = 'session.created' \
             AND resource_id = '{}'",
            session["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(created, ["1"]);
    assert!(
        users::session_events(&api, "session.created")
            .await
            .contains(&format!(
                "{} {}",
                ada.user_id,
                session["id"].as_str().unwrap()
            ))
    );
}

#[tokio::test]
async fn auth_059_a_completion_replaces_its_device_s_session_and_ends_the_device_token_it_used() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let first = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD, "device_id": "laptop" }),
        )
        .send()
        .await
        .json();
    let session = complete(
        &api,
        first["challenge"].as_str().unwrap(),
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await
    .json();
    let device_token = session["device_token"].as_str().unwrap().to_owned();

    // A sign-in on the same device with its device token replaces that session, and ends the
    // token, as a sign-in without an app does (AUTH-008, AUTH-019).
    let again = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD, "device_id": "laptop",
                     "device_token": device_token }),
        )
        .send()
        .await
        .json();
    let held = reads(
        &api,
        "SELECT (device_token_hash IS NOT NULL)::text FROM sign_in_challenges",
    )
    .await;
    assert_eq!(held, ["true"]);
    let replaced = complete(
        &api,
        again["challenge"].as_str().unwrap(),
        json!({ "recovery_code": app.recovery_codes[0] }),
    )
    .await;
    assert_eq!(replaced.status, 201, "{}", replaced.json());
    let on_laptop = reads(
        &api,
        "SELECT id::text FROM sessions WHERE device_id = 'laptop'",
    )
    .await;
    assert_eq!(on_laptop, [replaced.json()["id"].as_str().unwrap()]);
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM recovery_codes").await,
        ["9"]
    );
    let used = sha_of_device_token(&device_token);
    assert!(
        reads(
            &api,
            &format!(
                "SELECT id::text FROM device_tokens WHERE token_hash = decode('{used}', 'hex')"
            )
        )
        .await
        .is_empty()
    );
}

/// The SHA-256 of a device token, in hexadecimal.
fn sha_of_device_token(token: &str) -> String {
    hex_sha256(token)
}

#[tokio::test]
async fn auth_059_of_two_completions_of_one_challenge_only_one_starts_a_session() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let token = challenge(&api, "ada@example.com").await;
    let codes = [app.recovery_codes[0].clone(), app.recovery_codes[1].clone()];

    let (first, second) = tokio::join!(
        complete(&api, &token, json!({ "recovery_code": codes[0] })),
        complete(&api, &token, json!({ "recovery_code": codes[1] })),
    );

    let mut statuses = [first.status, second.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [201, 401]);
    let refused = if first.status == 401 { first } else { second };
    assert_eq!(refused.json()["code"], "auth.invalid_challenge");
    // The losing completion rolled back its recovery code's use.
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM recovery_codes").await,
        ["9"]
    );
}

#[tokio::test]
async fn auth_060_an_unknown_expired_or_used_challenge_answers_invalid_challenge() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let unknown = complete(&api, "nothing", json!({ "code": "123456" })).await;
    assert_eq!(unknown.status, 401);
    assert_eq!(unknown.json()["code"], "auth.invalid_challenge");

    let token = challenge(&api, "ada@example.com").await;
    api.database()
        .owner_executes("UPDATE sign_in_challenges SET expires_at = now() - interval '1 second'")
        .await;
    let expired = complete(
        &api,
        &token,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;
    assert_eq!(expired.json()["code"], "auth.invalid_challenge");

    let token = challenge(&api, "ada@example.com").await;
    let done = complete(
        &api,
        &token,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;
    assert_eq!(done.status, 201);
    forget_steps(&api, &ada.user_id).await;
    let used = complete(
        &api,
        &token,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;
    assert_eq!(used.status, 401);
    assert_eq!(used.json()["code"], "auth.invalid_challenge");
}

#[tokio::test]
async fn auth_060_a_wrong_code_answers_401_records_it_and_leaves_the_challenge() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let token = challenge(&api, "ada@example.com").await;
    let right = code(&api, &app.secret).await;

    let wrong = complete(&api, &token, json!({ "code": other_code(&right) })).await;

    assert_eq!(wrong.status, 401);
    assert_eq!(wrong.json()["code"], "auth.invalid_second_factor");
    let ada_id = &ada.user_id;
    assert_eq!(
        reads(
            &api,
            "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') FROM audit_entries \
             WHERE action = 'second_factor.check_failed'"
        )
        .await,
        [format!("{ada_id} -")]
    );
    let recovery = complete(
        &api,
        &token,
        json!({ "recovery_code": "7KQM-2XWD-9RTB-H4NP-1100-ABCD" }),
    )
    .await;
    assert_eq!(recovery.json()["code"], "auth.invalid_second_factor");
    for (body, pointer) in [
        (json!({}), "/code"),
        (
            json!({ "code": right, "recovery_code": app.recovery_codes[0] }),
            "/code",
        ),
        (json!({ "code": "12345" }), "/code"),
        (json!({ "recovery_code": "ABCD" }), "/recovery_code"),
    ] {
        let mut body = body;
        body["challenge"] = json!(token);
        let invalid = api
            .post_raw("/v1/sessions/second-factor", &body)
            .send()
            .await;
        assert_eq!(invalid.status, 400);
        assert_eq!(invalid.json()["errors"][0]["field"], pointer);
    }
    let completed = complete(&api, &token, json!({ "code": right })).await;
    assert_eq!(completed.status, 201, "{}", completed.json());
}

#[tokio::test]
async fn auth_061_completions_count_in_the_user_s_bucket_or_their_device_token_s() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    // A first completion, whose device token the device keeps (AUTH-019).
    let first = challenge(&api, "ada@example.com").await;
    let session = complete(
        &api,
        &first,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;
    assert_eq!(session.status, 201, "{}", session.json());
    let device_token = session.json()["device_token"].as_str().unwrap().to_owned();

    let token = challenge(&api, "ada@example.com").await;
    let right = code(&api, &app.secret).await;
    for _ in 0..9 {
        let wrong = complete(&api, &token, json!({ "code": other_code(&right) })).await;
        assert_eq!(wrong.status, 401);
    }
    let limited = complete(&api, &token, json!({ "code": right })).await;
    assert_eq!(limited.status, 429);
    assert_eq!(limited.json()["code"], "request.rate_limited");
    assert!(
        limited
            .header("ratelimit-policy")
            .starts_with("\"second_factor_sign_in\";q=10;w=86400"),
        "{}",
        limited.header("ratelimit-policy")
    );

    // A challenge that holds a device token valid for the email counts in that token's bucket,
    // which no completion from elsewhere fills.
    let trusted = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD,
                     "device_token": device_token }),
        )
        .send()
        .await
        .json()["challenge"]
        .as_str()
        .unwrap()
        .to_owned();
    forget_steps(&api, &ada.user_id).await;
    let completed = complete(
        &api,
        &trusted,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;
    assert_eq!(completed.status, 201, "{}", completed.json());
}

#[tokio::test]
async fn auth_061_a_signed_in_user_s_checks_of_codes_count_in_one_bucket_of_theirs() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let secret = crate::support::second_factors::start_setup(&api, &ada)
        .await
        .json()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let right = code(&api, &secret).await;
    for _ in 0..10 {
        let wrong = crate::support::second_factors::confirm(&api, &ada, &other_code(&right)).await;
        assert_eq!(wrong.status, 422);
    }

    let limited = crate::support::second_factors::confirm(&api, &ada, &right).await;
    assert_eq!(limited.status, 429);
    assert!(
        limited
            .header("ratelimit-policy")
            .starts_with("\"second_factor_check\";q=10;w=86400"),
        "{}",
        limited.header("ratelimit-policy")
    );
    // A re-authentication that carries a code counts in the same bucket, before its password's.
    let reauthenticated = api
        .post(
            "/v1/users/me/sessions/current/reauthenticate",
            &json!({ "password": PASSWORD, "code": right }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(reauthenticated.status, 429);
    // Another user's bucket is their own.
    add_app(&api, &bob).await;
}

#[tokio::test]
async fn auth_066_an_app_s_secret_is_stored_encrypted_and_moves_to_the_current_key_when_checked() {
    let database = TestDatabase::migrated().await;
    let first = Options::default().set("ENCRYPTION_KEYS", format!("1:{}", key(1)));
    let api = Api::on(database, first).await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let stored = reads(
        &api,
        "SELECT key_version || ' ' || encode(secret, 'hex') FROM totp_factors",
    )
    .await;
    let bytes = data_encoding::BASE32_NOPAD
        .decode(app.secret.as_bytes())
        .unwrap();
    let plain: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    assert!(stored[0].starts_with("1 "), "{}", stored[0]);
    assert!(!stored[0].contains(&plain));

    // A ring with a second, current key opens the old secret, and stores it again under the new
    // key once a check opens it.
    let both = Options::default().set("ENCRYPTION_KEYS", format!("1:{},2:{}", key(1), key(2)));
    let next = api.instance(both).await;
    let token = challenge(&next, "ada@example.com").await;
    let completed = complete(
        &next,
        &token,
        json!({ "code": code(&next, &app.secret).await }),
    )
    .await;
    assert_eq!(completed.status, 201, "{}", completed.json());
    let versions = reads(&next, "SELECT key_version::text FROM totp_factors").await;
    assert_eq!(versions, ["2"]);
}

#[tokio::test]
async fn auth_066_the_hourly_job_moves_every_secret_to_the_current_key() {
    let database = TestDatabase::migrated().await;
    let api = Api::on(
        database,
        Options::default().set("ENCRYPTION_KEYS", format!("1:{}", key(1))),
    )
    .await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let bob = users::register(&api, "bob@example.com").await;
    add_app(&api, &bob).await;

    let next = api
        .instance(Options::default().set("ENCRYPTION_KEYS", format!("1:{},2:{}", key(1), key(2))))
        .await;
    next.run_periodic("auth.reencrypt_secrets").await;

    let versions = reads(
        &next,
        "SELECT key_version::text FROM totp_factors ORDER BY 1",
    )
    .await;
    assert_eq!(versions, ["2", "2"]);
    // The moved secret still gives the app's codes, under a ring without the old key.
    let only = next
        .instance(Options::default().set("ENCRYPTION_KEYS", format!("2:{}", key(2))))
        .await;
    let token = challenge(&only, "ada@example.com").await;
    let completed = complete(
        &only,
        &token,
        json!({ "code": code(&only, &app.secret).await }),
    )
    .await;
    assert_eq!(completed.status, 201, "{}", completed.json());
}

#[tokio::test]
async fn auth_066_serve_refuses_a_ring_without_a_version_that_a_stored_secret_needs() {
    let database = TestDatabase::migrated().await;
    let user = users::without_password(&database, "ada@example.com").await;
    database
        .owner_executes(&format!(
            "INSERT INTO totp_factors (user_id, key_version, secret, last_used_step) \
             VALUES ('{user}', 9, '\\x00', 0)"
        ))
        .await;

    let output = database
        .run_api(&["serve"], &[("ENCRYPTION_KEYS", format!("1:{}", key(1)))])
        .await;

    assert!(!output.status.success());
    let printed = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(
        printed.contains("ENCRYPTION_KEYS lacks version 9"),
        "{printed}"
    );
}

#[tokio::test]
async fn auth_067_the_hourly_job_deletes_the_setups_and_challenges_that_expired() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    let lasting = challenge(&api, "ada@example.com").await;
    challenge(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 0).await;
    let setup = crate::support::second_factors::start_setup(&api, &ada).await;
    assert_eq!(setup.status, 200);
    api.database()
        .owner_executes(&format!(
            "UPDATE sign_in_challenges SET expires_at = now() - interval '1 second' \
             WHERE token_hash <> decode('{}', 'hex');
            UPDATE totp_setups SET expires_at = now() - interval '1 second'",
            hex_sha256(&lasting)
        ))
        .await;

    api.run_periodic("auth.expire_unfinished").await;

    assert_eq!(
        reads(
            &api,
            "SELECT encode(token_hash, 'hex') FROM sign_in_challenges"
        )
        .await,
        [hex_sha256(&lasting)]
    );
    assert!(
        reads(&api, "SELECT id::text FROM totp_setups")
            .await
            .is_empty()
    );
    assert!(
        reads(
            &api,
            "SELECT id::text FROM audit_entries WHERE action LIKE 'sign_in.%' \
             AND created_at > now() - interval '1 second' AND action <> 'sign_in.challenged'"
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
async fn auth_069_a_change_of_the_password_deletes_the_challenges_issued_before_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let before = challenge(&api, "ada@example.com").await;

    let changed = api
        .post(
            "/v1/users/me/password/change",
            &json!({ "current_password": PASSWORD, "password": "violet-harbor-73-brisk",
                     "code": code(&api, &app.secret).await }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(changed.status, 204, "{}", changed.json());
    forget_steps(&api, &ada.user_id).await;
    let stale = complete(
        &api,
        &before,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;
    assert_eq!(stale.json()["code"], "auth.invalid_challenge");
}

#[tokio::test]
async fn auth_069_a_reset_deletes_the_challenges_and_leaves_the_app_that_the_next_sign_in_asks_for()
{
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    add_app(&api, &ada).await;
    challenge(&api, &address).await;
    let asked = api
        .post("/v1/password-resets", &json!({ "email": address }))
        .send()
        .await;
    assert_eq!(asked.status, 202);
    api.run_jobs().await;
    let emails = mailbox::to(&address).await;
    let email = emails
        .iter()
        .find(|email| email.subject.contains("password reset code"))
        .expect("an email with a reset code");
    let reset_code = mailbox::grouped_code_in(&email.text).expect("a reset code");

    let reset = api
        .post(
            "/v1/password-resets/confirm",
            &json!({ "email": address, "code": reset_code, "password": "violet-harbor-73-brisk" }),
        )
        .send()
        .await;

    assert_eq!(reset.status, 204, "{}", reset.json());
    assert!(
        reads(&api, "SELECT id::text FROM sign_in_challenges")
            .await
            .is_empty()
    );
    // The app stays, so the next sign-in asks for it (ASVS 6.4.3).
    assert_eq!(
        users::sign_in(&api, &address, "violet-harbor-73-brisk")
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn auth_069_a_deletion_of_the_account_deletes_every_row_of_its_second_factors() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    challenge(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 0).await;
    let setup = crate::support::second_factors::start_setup(&api, &ada).await;
    assert_eq!(setup.status, 200);

    let deleted = api
        .request(http::Method::DELETE, "/v1/users/me")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(deleted.status, 204);
    for table in [
        "totp_factors",
        "totp_setups",
        "recovery_codes",
        "sign_in_challenges",
    ] {
        assert!(
            reads(&api, &format!("SELECT id::text FROM {table}"))
                .await
                .is_empty(),
            "{table}"
        );
    }
}
