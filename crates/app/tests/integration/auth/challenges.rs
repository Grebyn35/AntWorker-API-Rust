//! Signing in to an account with an authenticator app: its challenge, and the completion that a
//! code gives; their limits; the jobs of second factors; and the encryption of apps' secrets (the
//! auth spec's "Second factors").

use opentelemetry_proto::tonic::trace::v1::Span;
use platform::telemetry::testing::collector::Collector;
use serde_json::{Value, json};

use crate::support::{
    Api, Options, Server, TestDatabase,
    api::reads,
    children, execute_on, is_the_request_s, limited_while_holding, mailbox,
    operator::printed,
    password_reset, providers,
    second_factors::{
        self, add_app, challenge, code, code_at, complete, forget_steps, other_code,
        second_factor_rows, with_a_challenge,
    },
    users::{self, PASSWORD, SignedIn, WEB},
};

/// The base64 of 32 bytes of `byte`, a key of the key ring.
fn key(byte: u8) -> String {
    data_encoding::BASE64.encode(&[byte; 32])
}

/// The IDs of `user`'s sign-in challenges.
async fn challenges_of(api: &Api, user: &SignedIn) -> Vec<String> {
    reads(
        api,
        &format!(
            "SELECT id::text FROM sign_in_challenges WHERE user_id = '{}'",
            user.user_id
        ),
    )
    .await
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
        body["expires_at"],
        reads(
            &api,
            "SELECT to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
             FROM sign_in_challenges"
        )
        .await[0]
    );
    let ada_id = &ada.user_id;
    assert_eq!(
        reads(
            &api,
            "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_type \
             || ' ' || resource_id || ' ' || details::text FROM audit_entries \
             WHERE action = 'sign_in.challenged'"
        )
        .await,
        [format!("{ada_id} - user {ada_id} {{}}")]
    );
}

/// The SHA-256 of `token`, in hexadecimal.
fn hex_sha256(token: &str) -> String {
    crate::support::hex(&platform::hashing::sha256(token.as_bytes()))
}

#[tokio::test]
async fn auth_058_a_challenge_holds_no_device_token_that_isn_t_valid_for_the_email() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    let grace = api
        .post("/v1/users", &users::registration("grace@example.com"))
        .send()
        .await;
    let graces = grace.json()["device_token"].as_str().unwrap().to_owned();
    // An unknown token of 64 characters, the most that the field takes.
    let unknown = "x".repeat(64);

    for token in [&graces, &unknown] {
        let response = api
            .post(
                "/v1/sessions",
                &json!({ "email": "ada@example.com", "password": PASSWORD,
                         "device_token": token }),
            )
            .send()
            .await;
        assert_eq!(response.status, 200, "{}", response.json());
    }

    assert_eq!(
        reads(
            &api,
            "SELECT (device_token_hash IS NULL)::text FROM sign_in_challenges"
        )
        .await,
        ["true", "true"],
        "another email's token and an unknown one"
    );
}

#[tokio::test]
async fn auth_058_a_browser_takes_the_challenge_in_a_cookie() {
    let api = users::browsers_api().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;

    let response = users::sign_in_from_browser(&api, "ada@example.com", "").await;

    assert_eq!(response.status, 200);
    assert!(response.json().get("challenge").is_none());
    let set = response
        .headers("set-cookie")
        .into_iter()
        .find(|set| set.starts_with("__Host-sign_in_challenge="))
        .expect("the challenge's cookie")
        .to_owned();
    assert!(
        set.ends_with("; Path=/; Secure; HttpOnly; SameSite=Strict"),
        "{set}"
    );
    assert!(!set.contains("Domain"), "{set}");
    // It expires with the challenge, to the second.
    let expires = reads(
        &api,
        "SELECT to_char(expires_at AT TIME ZONE 'UTC', 'Dy, DD Mon YYYY HH24:MI:SS \"GMT\"') \
         FROM sign_in_challenges",
    )
    .await;
    assert_eq!(users::expires(&response, "sign_in_challenge"), expires[0]);
    // Its value is the challenge's token.
    let cookie = users::cookie(&response, "sign_in_challenge");
    let hash = reads(
        &api,
        "SELECT encode(token_hash, 'hex') FROM sign_in_challenges",
    )
    .await;
    assert_eq!(hash, [hex_sha256(&cookie)]);
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
            "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_type \
             || ' ' || details::text FROM audit_entries WHERE action = 'session.created' \
             AND resource_id = '{}'",
            session["id"].as_str().unwrap()
        ),
    )
    .await;
    let ada_id = &ada.user_id;
    assert_eq!(created, [format!("{ada_id} {ada_id} session {{}}")]);
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
async fn auth_059_a_completion_starts_the_session_on_the_challenge_s_device_with_its_provider() {
    let (upstream, api) = providers::standing().await;
    providers::gives(
        &upstream,
        providers::identity("subject-1", "ada@example.com", true),
    );
    let ada = providers::signed_in(&api).await;
    let app = add_app(&api, &ada).await;
    let mut grant = providers::grant();
    grant["device_id"] = json!("laptop");
    grant["device_name"] = json!("Ada's laptop");
    let challenged = providers::sign_in(&api, &grant).await.json();

    let completed = complete(
        &api,
        challenged["challenge"].as_str().unwrap(),
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;

    assert_eq!(completed.status, 201, "{}", completed.json());
    let session = completed.json();
    assert_eq!(session["device_id"], "laptop");
    assert_eq!(session["device_name"], "Ada's laptop");
    assert_eq!(session["provider"], "stand_in");
}

#[tokio::test]
async fn auth_059_a_browser_s_cookie_carries_the_challenge_and_the_completion_clears_it() {
    let api = users::browsers_api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let challenged = users::sign_in_from_browser(&api, "ada@example.com", "").await;
    let cookie = users::cookie(&challenged, "sign_in_challenge");

    // The body holds no challenge.
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
    let cleared = completed
        .headers("set-cookie")
        .into_iter()
        .any(|set| set.starts_with("__Host-sign_in_challenge=;") && set.contains("Max-Age=0"));
    assert!(cleared, "{:?}", completed.headers("set-cookie"));
}

#[tokio::test]
async fn auth_059_a_completion_whose_body_holds_a_challenge_completes_it_and_not_the_cookie_s() {
    let api = users::browsers_api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let challenged = users::sign_in_from_browser(&api, "ada@example.com", "").await;
    let in_cookie = users::cookie(&challenged, "sign_in_challenge");
    let in_body = challenge(&api, "ada@example.com").await;

    let completed = api
        .post(
            "/v1/sessions/second-factor",
            &json!({ "challenge": in_body, "code": code(&api, &app.secret).await }),
        )
        .header("origin", WEB)
        .header("cookie", &format!("__Host-sign_in_challenge={in_cookie}"))
        .send()
        .await;

    assert_eq!(completed.status, 201, "{}", completed.json());
    // The body's challenge completed, and the cookie's still waits.
    assert_eq!(
        reads(
            &api,
            "SELECT encode(token_hash, 'hex') FROM sign_in_challenges"
        )
        .await,
        [hex_sha256(&in_cookie)]
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
    let counts = "SELECT id || ' ' || count FROM rate_limit_buckets ORDER BY id";
    let before = reads(&api, counts).await;
    let expired = complete(
        &api,
        &token,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;
    assert_eq!(expired.json()["code"], "auth.invalid_challenge");
    // The pipeline counts the request in the IP limit; no bucket of the user's or of a device
    // token's counts it, since it checks no code (AUTH-061).
    let after = reads(&api, counts).await;
    assert_eq!(after.len(), before.len());
    let changed = after.iter().filter(|row| !before.contains(row)).count();
    assert_eq!(changed, 1, "{before:?} then {after:?}");

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
async fn auth_060_a_challenge_that_is_empty_or_longer_than_64_characters_answers_400_at_its_field()
{
    let api = Api::new().await;
    let (longest, too_long) = ("x".repeat(64), "x".repeat(65));

    let unknown = api
        .post_raw(
            "/v1/sessions/second-factor",
            &json!({ "challenge": longest, "code": "123456" }),
        )
        .send()
        .await;
    for token in ["", too_long.as_str()] {
        let response = api
            .post_raw(
                "/v1/sessions/second-factor",
                &json!({ "challenge": token, "code": "123456" }),
            )
            .send()
            .await;

        assert_eq!(response.status, 400, "{token}: {}", response.json());
        assert_eq!(
            response.json()["errors"],
            json!([{
                "field": "/challenge",
                "code": "invalid_challenge",
                "message": "The token must be 1 to 64 characters.",
            }])
        );
    }
    assert_eq!(unknown.status, 401);
    assert_eq!(unknown.json()["code"], "auth.invalid_challenge");
}

#[tokio::test]
async fn auth_060_an_empty_challenge_and_a_malformed_code_answer_400_with_both_fields() {
    let api = Api::new().await;

    let refused = api
        .post_raw(
            "/v1/sessions/second-factor",
            &json!({ "challenge": "", "code": "12345" }),
        )
        .send()
        .await;

    assert_eq!(
        refused.refused_fields(),
        [
            ("/challenge", "invalid_challenge"),
            ("/code", "invalid_code")
        ]
        .map(|(field, code)| (field.to_owned(), code.to_owned()))
    );
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
            "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_type \
             || ' ' || resource_id || ' ' || details::text FROM audit_entries \
             WHERE action = 'second_factor.check_failed'"
        )
        .await,
        [format!("{ada_id} - user {ada_id} {{}}")]
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
        assert_eq!(
            invalid.refused_fields(),
            [(pointer.to_owned(), "invalid_code".to_owned())],
            "{body}"
        );
    }
    let completed = complete(&api, &token, json!({ "code": right })).await;
    assert_eq!(completed.status, 201, "{}", completed.json());
}

#[tokio::test]
async fn auth_060_a_code_of_more_than_32_characters_answers_400_at_its_field() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let token = challenge(&api, "ada@example.com").await;
    // The right code, which the spaces before it take past 32 characters.
    let long = format!("{}{}", " ".repeat(27), code(&api, &app.secret).await);

    let refused = api
        .post_raw(
            "/v1/sessions/second-factor",
            &json!({ "challenge": token, "code": long }),
        )
        .send()
        .await;

    assert_eq!(refused.status, 400, "{}", refused.json());
    assert_eq!(refused.json()["errors"][0]["field"], "/code");
    assert_eq!(refused.json()["errors"][0]["code"], "invalid_code");
}

#[tokio::test]
async fn auth_060_a_code_of_32_characters_with_the_whitespace_around_it_completes_the_sign_in() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let token = challenge(&api, "ada@example.com").await;
    // The right code, which the spaces before it take to 32 characters.
    let padded = format!("{}{}", " ".repeat(26), code(&api, &app.secret).await);

    let completed = complete(&api, &token, json!({ "code": padded })).await;

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
    let secret = second_factors::start_setup(&api, &ada).await.json()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let right = code(&api, &secret).await;
    for _ in 0..10 {
        let wrong = second_factors::confirm(&api, &ada, &other_code(&right)).await;
        assert_eq!(wrong.status, 422);
    }

    let limited = second_factors::confirm(&api, &ada, &right).await;
    assert_eq!(limited.status, 429);
    assert!(
        limited
            .header("ratelimit-policy")
            .starts_with("\"second_factor_check\";q=10;w=86400"),
        "{}",
        limited.header("ratelimit-policy")
    );
    // A re-authentication that carries a code counts in the same bucket, before its password's.
    let reauthenticated =
        second_factors::reauthenticate(&api, &ada, &json!({ "password": PASSWORD, "code": right }))
            .await;
    assert_eq!(reauthenticated.status, 429);
    // So does a change of the password that carries a code, before its password's bucket.
    let changed = second_factors::change_password(
        &api,
        &ada,
        json!({ "current_password": PASSWORD, "password": "violet-harbor-73-brisk", "code": right }),
    )
    .await;
    assert_eq!(changed.status, 429);
    assert!(
        changed
            .header("ratelimit-policy")
            .starts_with("\"second_factor_check\""),
        "{}",
        changed.header("ratelimit-policy")
    );
    // The 13 requests counted in the bucket of codes, which refused the last 3, so the 2 that
    // carry a password never reached the password's bucket.
    assert_eq!(
        users::counted_in(&api, "second_factor_check", &ada).await,
        Some(13)
    );
    assert_eq!(users::counted_in(&api, "password_check", &ada).await, None);
    // Another user's bucket is their own.
    add_app(&api, &bob).await;
}

#[tokio::test]
async fn auth_061_a_check_of_a_code_beyond_its_limit_answers_429_before_its_unit_of_work_takes_the_user_s_turn()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let secret = second_factors::start_setup(&api, &ada).await.json()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let right = code(&api, &secret).await;
    for _ in 0..10 {
        let wrong = second_factors::confirm(&api, &ada, &other_code(&right)).await;
        assert_eq!(wrong.status, 422);
    }

    // Held against the user's turn, which the confirmation's unit of work takes first.
    limited_while_holding(
        api.database(),
        &format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR UPDATE",
            ada.user_id
        ),
        second_factors::confirm(&api, &ada, &right),
    )
    .await;
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
    let plain = crate::support::hex(&bytes);
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
async fn auth_066_a_run_of_the_job_moves_the_secrets_of_more_apps_than_one_batch_holds() {
    let database = TestDatabase::migrated().await;
    // 1,001 apps whose secret the old key holds, one more than a batch of 1,000.
    let users = users::many_with_a_session(&database).await;
    crate::support::second_factors::apps_sealed_by(&database, &users, &format!("1:{}", key(1)))
        .await;
    let api = Api::on(
        database,
        Options::default().set("ENCRYPTION_KEYS", format!("1:{},2:{}", key(1), key(2))),
    )
    .await;

    api.run_periodic("auth.reencrypt_secrets").await;

    assert_eq!(
        reads(
            &api,
            "SELECT key_version || ' ' || count(*) FROM totp_factors GROUP BY key_version"
        )
        .await,
        ["2 1001"]
    );
    // How many rows each transaction of the run wrote, as the transaction's ID that each row's
    // `xmin` holds groups them: a batch of 1,000, then the one left.
    assert_eq!(
        reads(
            &api,
            "SELECT count(*)::text FROM totp_factors GROUP BY xmin::text ORDER BY count(*) DESC"
        )
        .await,
        ["1000", "1"]
    );
}

#[tokio::test]
async fn auth_066_the_job_that_moves_the_secrets_runs_every_hour() {
    let api = Api::new().await;

    let interval = api.interval("auth.reencrypt_secrets");

    assert_eq!(interval, crate::support::jobs::HOUR);
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
    let stored = "SELECT key_version || ' ' || encode(secret, 'hex') FROM totp_factors ORDER BY 1";
    let moved = reads(&next, stored).await;
    next.run_periodic("auth.reencrypt_secrets").await;

    // A second run finds nothing more to move, and changes nothing.
    assert_eq!(reads(&next, stored).await, moved);
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
async fn auth_066_an_app_that_a_request_holds_is_left_for_the_next_run() {
    let database = TestDatabase::migrated().await;
    let api = Api::on(
        database,
        Options::default().set("ENCRYPTION_KEYS", format!("1:{}", key(1))),
    )
    .await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    let bob = users::register(&api, "bob@example.com").await;
    add_app(&api, &bob).await;
    let next = api
        .instance(Options::default().set("ENCRYPTION_KEYS", format!("1:{},2:{}", key(1), key(2))))
        .await;
    let mut request = next
        .database()
        .holding(&format!(
            "SELECT 1 FROM totp_factors WHERE user_id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;
    let versions = format!(
        "SELECT (user_id = '{}')::text || ' ' || key_version FROM totp_factors ORDER BY 1",
        ada.user_id
    );

    // The run waits for no lock, so it ends while the row stays held.
    next.run_periodic("auth.reencrypt_secrets").await;
    let during = reads(&next, &versions).await;
    execute_on(&mut request, "COMMIT").await;
    next.run_periodic("auth.reencrypt_secrets").await;

    assert_eq!(during, ["false 2", "true 1"]);
    assert_eq!(reads(&next, &versions).await, ["false 2", "true 2"]);
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
        .run_api(
            &["serve"],
            &[
                ("ENCRYPTION_KEYS", format!("1:{}", key(1))),
                // A port of its own, beside the other tests that run serve.
                ("LISTEN_ADDR", "127.0.0.1:0".to_owned()),
            ],
        )
        .await;

    assert!(!output.status.success());
    let written = printed(&output);
    assert!(
        written.contains("ENCRYPTION_KEYS lacks version 9"),
        "{written}"
    );
}

#[tokio::test]
async fn auth_067_the_job_runs_every_hour() {
    let api = Api::new().await;

    let interval = api.interval("auth.expire_unfinished");

    assert_eq!(interval, crate::support::jobs::HOUR);
}

#[tokio::test]
async fn auth_067_the_hourly_job_deletes_the_setups_and_challenges_that_expired() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    add_app(&api, &ada).await;
    let lasting = challenge(&api, "ada@example.com").await;
    challenge(&api, "ada@example.com").await;
    let setup = second_factors::start_setup(&api, &ada).await;
    assert_eq!(setup.status, 200);
    let kept = second_factors::start_setup(&api, &bob).await;
    assert_eq!(kept.status, 200);
    api.database()
        .owner_executes(&format!(
            "UPDATE sign_in_challenges SET expires_at = now() - interval '1 second' \
             WHERE token_hash <> decode('{}', 'hex');
            UPDATE totp_setups SET expires_at = now() - interval '1 second' \
             WHERE user_id = '{}'",
            hex_sha256(&lasting),
            ada.user_id
        ))
        .await;
    let db = api.database();
    db.owner_executes("DELETE FROM events").await;
    let entries = "SELECT count(*)::text FROM audit_entries";
    let before = reads(&api, entries).await;
    let mut signals = db.signals().await;
    let challenges = "SELECT encode(token_hash, 'hex') FROM sign_in_challenges";
    let setups = "SELECT user_id::text FROM totp_setups";

    api.run_periodic("auth.expire_unfinished").await;
    let once = (reads(&api, challenges).await, reads(&api, setups).await);
    api.run_periodic("auth.expire_unfinished").await;

    assert_eq!(once.0, [hex_sha256(&lasting)]);
    assert_eq!(once.1, std::slice::from_ref(&bob.user_id));
    // A second run finds nothing more to delete.
    assert_eq!(reads(&api, challenges).await, once.0);
    assert_eq!(reads(&api, setups).await, once.1);
    // Scheduled maintenance records, publishes and signals nothing.
    assert_eq!(reads(&api, entries).await, before);
    assert!(db.owner_reads("SELECT name FROM events").await.is_empty());
    assert!(signals.received().await.is_empty());
}

#[tokio::test]
async fn auth_067_a_run_that_meets_a_row_that_a_request_holds_ends_there_and_the_next_deletes_the_rest()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    let held = challenge(&api, "ada@example.com").await;
    // Another challenge, which no request holds, and which expires with the first.
    challenge(&api, "ada@example.com").await;
    api.database()
        .owner_executes("UPDATE sign_in_challenges SET expires_at = now() - interval '1 second'")
        .await;
    // A request that deletes the challenge, as a reset deletes its user's, and hasn't committed.
    let mut request = api
        .database()
        .holding(&format!(
            "DELETE FROM sign_in_challenges WHERE token_hash = decode('{}', 'hex')",
            hex_sha256(&held)
        ))
        .await;

    // The run waits at most a millisecond for the held row, then ends while the request holds
    // it, rolling its batch back, so the other row stays too.
    api.run_periodic("auth.expire_unfinished").await;
    execute_on(&mut request, "ROLLBACK").await;
    let during = reads(&api, "SELECT count(*)::text FROM sign_in_challenges").await;
    api.run_periodic("auth.expire_unfinished").await;

    assert_eq!(during, ["2"]);
    assert!(
        reads(&api, "SELECT id::text FROM sign_in_challenges")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_067_a_run_deletes_more_expired_setups_than_a_batch_holds() {
    let api = Api::new().await;
    // 1,001 accounts, each with a setup that expired, one more than the job's batch of 1,000.
    let users = users::many_with_a_session(api.database()).await;
    api.database()
        .owner_executes(&format!(
            "INSERT INTO totp_setups (user_id, key_version, secret, expires_at) \
             SELECT id, 1, '\\x00', now() - interval '1 second' \
             FROM unnest('{{{}}}'::uuid[]) AS id",
            users.join(",")
        ))
        .await;

    api.run_periodic("auth.expire_unfinished").await;

    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM totp_setups").await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_067_a_run_deletes_more_expired_challenges_than_a_batch_holds() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // 1,001 challenges of Ada's that expired, one more than the job's batch of 1,000.
    api.database()
        .owner_executes(&format!(
            "INSERT INTO sign_in_challenges (user_id, token_hash, expires_at) \
             SELECT '{}', sha256(('challenge ' || n)::bytea), now() - interval '1 second' \
             FROM generate_series(1, 1001) AS n",
            ada.user_id
        ))
        .await;

    api.run_periodic("auth.expire_unfinished").await;

    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM sign_in_challenges").await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_067_a_run_whose_batch_fails_but_for_a_held_row_fails() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    assert_eq!(second_factors::start_setup(&api, &ada).await.status, 200);
    let db = api.database();
    db.owner_executes("UPDATE totp_setups SET expires_at = now() - interval '1 second'")
        .await;
    // The deletion of a batch fails for want of the privilege that it needs, where only a row
    // that a request holds ends a run as done.
    db.owner_executes("REVOKE DELETE ON totp_setups FROM app")
        .await;

    let run = api.try_run_periodic("auth.expire_unfinished").await;

    assert!(run.is_err(), "the run ended as done");
    assert_eq!(
        reads(&api, "SELECT user_id::text FROM totp_setups").await,
        [ada.user_id.as_str()]
    );
}

#[tokio::test]
async fn auth_069_a_change_of_the_password_deletes_the_challenges_issued_before_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let before = challenge(&api, "ada@example.com").await;
    // A second challenge of hers, and another account's app, codes and challenge.
    challenge(&api, "ada@example.com").await;
    let bob = with_a_challenge(&api, "bob@example.com").await;
    let bobs = format!("WHERE user_id = '{}'", bob.user_id);
    let others = second_factor_rows(&api, &bobs).await;

    let changed = second_factors::change_password(
        &api,
        &ada,
        json!({ "current_password": PASSWORD, "password": "violet-harbor-73-brisk",
                "code": code(&api, &app.secret).await }),
    )
    .await;

    assert_eq!(changed.status, 200, "{}", changed.json());
    assert!(challenges_of(&api, &ada).await.is_empty());
    assert_eq!(second_factor_rows(&api, &bobs).await, others);
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
async fn auth_069_a_removal_of_the_app_deletes_the_challenges_issued_before_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let before = challenge(&api, "ada@example.com").await;
    let code_before = code(&api, &app.secret).await;
    // A second challenge of hers, and another account's app, codes and challenge.
    challenge(&api, "ada@example.com").await;
    let bob = with_a_challenge(&api, "bob@example.com").await;
    let bobs = format!("WHERE user_id = '{}'", bob.user_id);
    let others = second_factor_rows(&api, &bobs).await;

    let removed = second_factors::remove(&api, &ada).await;

    assert_eq!(removed.status, 204, "{}", removed.json());
    assert!(challenges_of(&api, &ada).await.is_empty());
    assert_eq!(second_factor_rows(&api, &bobs).await, others);
    let stale = complete(&api, &before, json!({ "code": code_before })).await;
    assert_eq!(stale.status, 401);
    assert_eq!(stale.json()["code"], "auth.invalid_challenge");
}

#[tokio::test]
async fn auth_069_a_reset_deletes_the_challenges_and_leaves_the_app_that_the_next_sign_in_asks_for()
{
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let app = add_app(&api, &ada).await;
    challenge(&api, &address).await;
    // A second challenge of hers, and another account's app, codes and challenge.
    challenge(&api, &address).await;
    let bob = with_a_challenge(&api, "bob@example.com").await;
    let bobs = format!("WHERE user_id = '{}'", bob.user_id);
    let others = second_factor_rows(&api, &bobs).await;
    let asked = password_reset::ask(&api, &address).await;
    assert_eq!(asked.status, 202);
    let reset_code = password_reset::code_sent(&api, &address).await;
    let codes = format!(
        "SELECT encode(code_hash, 'hex') FROM recovery_codes WHERE user_id = '{}' ORDER BY 1",
        ada.user_id
    );
    let recovery_codes = reads(&api, &codes).await;

    let reset = api
        .post(
            "/v1/password-resets/confirm",
            &json!({ "email": address, "code": reset_code, "password": "violet-harbor-73-brisk",
                     "second_factor_code": code(&api, &app.secret).await }),
        )
        .send()
        .await;

    assert_eq!(reset.status, 204, "{}", reset.json());
    assert!(challenges_of(&api, &ada).await.is_empty());
    assert_eq!(second_factor_rows(&api, &bobs).await, others);
    // The app and the recovery codes stay, so the next sign-in asks for one (ASVS 6.4.3).
    assert_eq!(reads(&api, &codes).await, recovery_codes);
    assert_eq!(recovery_codes.len(), 10);
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
    let ada = with_a_challenge(&api, "ada@example.com").await;
    // A second challenge of hers, and another account's app, codes and challenge.
    challenge(&api, "ada@example.com").await;
    let bob = with_a_challenge(&api, "bob@example.com").await;
    let bobs = format!("WHERE user_id = '{}'", bob.user_id);
    let others = second_factor_rows(&api, &bobs).await;
    users::authenticated_ago(&api, &ada, 0).await;
    let setup = second_factors::start_setup(&api, &ada).await;
    assert_eq!(setup.status, 200);

    let deleted = users::delete(&api, &ada).await;

    assert_eq!(deleted.status, 204);
    for table in [
        "totp_factors",
        "totp_setups",
        "recovery_codes",
        "sign_in_challenges",
    ] {
        assert!(
            reads(
                &api,
                &format!(
                    "SELECT id::text FROM {table} WHERE user_id = '{}'",
                    ada.user_id
                )
            )
            .await
            .is_empty(),
            "{table}"
        );
    }
    assert_eq!(second_factor_rows(&api, &bobs).await, others);
}

#[tokio::test]
async fn auth_046_a_completion_of_a_sign_in_publishes_session_created() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let token = challenge(&api, "ada@example.com").await;
    api.database().owner_executes("DELETE FROM events").await;

    let completed = complete(
        &api,
        &token,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;

    assert_eq!(completed.status, 201);
    assert_eq!(
        users::session_events(&api, "session.created").await,
        [format!(
            "{} {}",
            ada.user_id,
            completed.json()["id"].as_str().unwrap()
        )]
    );
}

#[tokio::test]
async fn auth_066_a_confirmation_stores_the_setup_s_secret_under_the_current_version() {
    let both = format!("1:{},2:{}", key(1), key(2));
    let api = Api::on(
        TestDatabase::migrated().await,
        Options::default()
            .set("ENCRYPTION_KEYS", &both)
            .set("ENCRYPTION_KEY_VERSION", "1"),
    )
    .await;
    let ada = users::register(&api, "ada@example.com").await;
    let setup = second_factors::start_setup(&api, &ada).await.json();
    assert_eq!(
        reads(&api, "SELECT key_version::text FROM totp_setups").await,
        ["1"]
    );
    let next = api
        .instance(Options::default().set("ENCRYPTION_KEYS", &both))
        .await;

    let confirmed = second_factors::confirm(
        &next,
        &ada,
        &code(&next, setup["secret"].as_str().unwrap()).await,
    )
    .await;

    assert_eq!(confirmed.status, 200, "{}", confirmed.json());
    assert_eq!(
        reads(&next, "SELECT key_version::text FROM totp_factors").await,
        ["2"]
    );
}

#[tokio::test]
async fn auth_066_serve_refuses_a_ring_without_a_lasting_setup_s_version_and_not_an_expired_one() {
    let database = TestDatabase::migrated().await;
    let user = users::without_password(&database, "ada@example.com").await;
    database
        .owner_executes(&format!(
            "INSERT INTO totp_setups (user_id, key_version, secret, expires_at) \
             VALUES ('{user}', 9, '\\x00', now() + interval '15 minutes')"
        ))
        .await;
    let ring = format!("1:{}", key(1));

    let lasting = database
        .run_api(
            &["serve"],
            &[
                ("ENCRYPTION_KEYS", ring.as_str()),
                ("LISTEN_ADDR", "127.0.0.1:0"),
            ],
        )
        .await;
    database
        .owner_executes("UPDATE totp_setups SET expires_at = now() - interval '1 second'")
        .await;
    // It starts, and listens, since no secret it may read needs the missing version.
    let started = database
        .serve(&[
            ("ENCRYPTION_KEYS", ring.as_str()),
            ("ENVIRONMENT", "production"),
        ])
        .await;

    assert!(!lasting.status.success());
    let written = printed(&lasting);
    assert!(
        written.contains("ENCRYPTION_KEYS lacks version 9"),
        "{written}"
    );
    drop(started);
}

/// POSTs `body`, as JSON, to `path` on `server`, with `headers`, and returns the answer's status
/// and its body as JSON.
async fn send(server: &Server, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, Value) {
    let mut headers = headers.to_vec();
    headers.push(("content-type", "application/json"));
    server.send("POST", path, &headers, body.as_bytes()).await
}

/// A `serve` on `database` that exports to `collector`, without email, so that an account is
/// served without proving its address (AUTH-025), and the bearer of Ada, who registered
/// there, with the code of the app whose setup she has started.
async fn set_up_through_serve(
    collector: &Collector,
    database: &TestDatabase,
) -> (Server, String, String) {
    let server = database.exporting_to(collector, &[("EMAIL", "off")]).await;
    let registration = users::registration("ada@example.com").to_string();
    let (_, session) = send(&server, "/v1/users", &[], &registration).await;
    let bearer = format!("Bearer {}", session["access_token"].as_str().unwrap());
    let setup_path = "/v1/users/me/second-factors/totp/setup";
    let (_, setup) = send(&server, setup_path, &[("authorization", &bearer)], "").await;
    let now: u64 = database
        .owner_reads("SELECT floor(extract(epoch FROM now()))::int8::text")
        .await[0]
        .parse()
        .unwrap();
    let code = code_at(setup["secret"].as_str().unwrap(), now);
    (server, bearer, code)
}

#[tokio::test]
async fn auth_061_a_confirmation_counts_its_code_before_its_unit_of_work_opens() {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let (server, bearer, code) = set_up_through_serve(&collector, &database).await;
    let trace = "5bf1a8b1a1e04e7a8c0f1d2e3a4b5c6d";

    let (confirmed, _) = send(
        &server,
        "/v1/users/me/second-factors/totp/setup/confirm",
        &[
            ("authorization", &bearer),
            ("traceparent", &format!("00-{trace}-b7ad6b7169203331-01")),
        ],
        &json!({ "code": code }).to_string(),
    )
    .await;
    // The request's span ends after the spans inside it, so its export follows theirs.
    let received = collector
        .wait_for(|received| {
            received
                .spans()
                .iter()
                .any(|(_, span)| is_the_request_s(span, trace))
        })
        .await;

    assert_eq!(confirmed, 200);
    let (_, request) = received
        .spans()
        .into_iter()
        .find(|(_, span)| is_the_request_s(span, trace))
        .unwrap();
    let within = children(&received, request);
    let named = |name: &str| -> Vec<&Span> {
        within
            .iter()
            .copied()
            .filter(|span| span.name == name)
            .collect()
    };
    // The unit of work is the last transaction to open; each count, the code's among them, has
    // ended before it.
    let unit_of_work = named("transaction")
        .into_iter()
        .max_by_key(|span| span.start_time_unix_nano)
        .unwrap();
    let counts = named("INSERT rate_limit_buckets");
    assert!(
        counts.len() >= 3,
        "the IP's, the user's and the code's: {within:?}"
    );
    for count in counts {
        assert!(
            count.end_time_unix_nano <= unit_of_work.start_time_unix_nano,
            "{count:?} before {unit_of_work:?}"
        );
    }
}

#[tokio::test]
async fn auth_061_a_completion_with_a_valid_device_token_counts_in_that_token_s_bucket() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let first = challenge(&api, "ada@example.com").await;
    let session = complete(
        &api,
        &first,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;
    let device_token = session.json()["device_token"].as_str().unwrap().to_owned();
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
    let right = code(&api, &app.secret).await;
    for _ in 0..10 {
        let wrong = complete(&api, &trusted, json!({ "code": other_code(&right) })).await;
        assert_eq!(wrong.status, 401);
    }

    let limited = complete(&api, &trusted, json!({ "code": right })).await;

    assert_eq!(limited.status, 429);
    assert!(
        limited
            .header("ratelimit-policy")
            .starts_with("\"second_factor_device\";q=10;w=86400"),
        "{}",
        limited.header("ratelimit-policy")
    );
    // The user's own bucket, which a completion without a valid token counts in, isn't full.
    let untrusted = challenge(&api, "ada@example.com").await;
    let completed = complete(&api, &untrusted, json!({ "code": right })).await;
    assert_eq!(completed.status, 201, "{}", completed.json());
}

#[tokio::test]
async fn auth_066_an_app_s_secret_moved_to_another_row_opens_nowhere() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let bob = users::register(&api, "bob@example.com").await;
    add_app(&api, &bob).await;
    // Bob's row takes Ada's ciphertext, as one who could write the database but holds no key
    // might move it, to sign in as Bob with Ada's app.
    api.database()
        .owner_executes(&format!(
            "UPDATE totp_factors SET secret = (SELECT secret FROM totp_factors \
             WHERE user_id = '{}') WHERE user_id = '{}'",
            ada.user_id, bob.user_id
        ))
        .await;
    let sessions = reads(&api, "SELECT count(*)::text FROM sessions").await;
    let token = challenge(&api, "bob@example.com").await;

    let completed = complete(
        &api,
        &token,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;

    assert_eq!(completed.status, 500);
    assert_eq!(completed.json()["code"], "request.internal_error");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM sessions").await,
        sessions
    );
}

#[tokio::test]
async fn auth_060_a_challenge_that_expired_while_its_completion_waited_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let token = challenge(&api, "ada@example.com").await;
    let right = code(&api, &app.secret).await;
    let db = api.database();
    let sessions = reads(&api, "SELECT count(*)::text FROM sessions").await;
    // A sign-in of Ada's holds her password identity, which the completion waits for.
    let mut sign_in = users::holding_identities(db, &ada).await;

    let completion = {
        let api = api.clone();
        let token = token.clone();
        tokio::spawn(async move { complete(&api, &token, json!({ "code": right })).await })
    };
    crate::support::queries_wait_on_locks(db, 1).await;
    db.owner_executes("UPDATE sign_in_challenges SET expires_at = now() - interval '1 second'")
        .await;
    execute_on(&mut sign_in, "COMMIT").await;
    let completed = completion.await.unwrap();

    assert_eq!(completed.status, 401);
    assert_eq!(completed.json()["code"], "auth.invalid_challenge");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM sessions").await,
        sessions
    );
}
