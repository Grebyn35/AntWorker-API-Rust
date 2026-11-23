//! A user's second factors: reading them, setting up an authenticator app, recovery codes,
//! removing the app, the codes that a signed-in user's re-authentication and change of the
//! password ask for, and the emails that tell the address (the auth spec's "Second factors").

use serde_json::{Value, json};

use crate::support::{
    Api,
    api::Response,
    api::reads,
    mailbox,
    providers::{self, gives, identity},
    second_factors::{self, App, add_app, code, confirm, forget_steps, other_code, start_setup},
    upstream::Upstream,
    users::{self, PASSWORD, SignedIn},
};

/// Each entry of `action`, as its log's user, its actor and its resource's type and ID.
async fn entries(api: &Api, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_type \
             || ' ' || coalesce(resource_id::text, '-') FROM audit_entries \
             WHERE action = '{action}' ORDER BY created_at"
        ))
        .await
}

/// Reads `user`'s second factors.
async fn read(api: &Api, user: &SignedIn) -> Response {
    api.get("/v1/users/me/second-factors")
        .bearer(&user.access)
        .send()
        .await
}

/// Asks for new recovery codes for `user`.
async fn new_codes(api: &Api, user: &SignedIn) -> Response {
    api.request(
        http::Method::POST,
        "/v1/users/me/second-factors/recovery-codes",
    )
    .bearer(&user.access)
    .send()
    .await
}

/// Removes `user`'s app.
async fn remove(api: &Api, user: &SignedIn) -> Response {
    api.request(http::Method::DELETE, "/v1/users/me/second-factors/totp")
        .bearer(&user.access)
        .send()
        .await
}

/// Re-authenticates `user` with `body`.
async fn reauthenticate(api: &Api, user: &SignedIn, body: &Value) -> Response {
    api.post("/v1/users/me/sessions/current/reauthenticate", body)
        .bearer(&user.access)
        .send()
        .await
}

#[tokio::test]
async fn auth_054_reading_one_s_second_factors_shows_the_app_and_the_codes_left_and_no_secret() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let none = read(&api, &ada).await;
    assert_eq!(none.status, 200);
    assert_eq!(
        none.json(),
        json!({ "totp": null, "recovery_codes_left": 0 })
    );

    let app = add_app(&api, &ada).await;
    let factors = read(&api, &ada).await.json();
    assert_eq!(factors["recovery_codes_left"], 10);
    let created = reads(
        &api,
        "SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
         FROM totp_factors",
    )
    .await;
    assert_eq!(factors["totp"]["created_at"], created[0].as_str());
    let used = reads(
        &api,
        "SELECT to_char(to_timestamp(last_used_step * 30) AT TIME ZONE 'UTC', \
         'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM totp_factors",
    )
    .await;
    assert_eq!(factors["totp"]["last_used_at"], used[0].as_str());
    assert!(!factors.to_string().contains(&app.secret));
    // Another user sees none of Ada's.
    let bob = users::register(&api, "bob@example.com").await;
    assert_eq!(read(&api, &bob).await.json()["totp"], Value::Null);
}

#[tokio::test]
async fn auth_054_the_codes_left_leave_out_a_recovery_code_that_was_used() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let proved = reauthenticate(
        &api,
        &ada,
        &json!({ "password": PASSWORD, "recovery_code": app.recovery_codes[0] }),
    )
    .await;
    assert_eq!(proved.status, 200, "{}", proved.json());

    // The proof gave the session new tokens (AUTH-095).
    let factors = read(&api, &SignedIn::from(&proved)).await;

    assert_eq!(factors.status, 200);
    assert_eq!(factors.json()["recovery_codes_left"], 9);
}

#[tokio::test]
async fn auth_055_a_setup_needs_a_recent_proof_and_answers_the_secret_once() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 10).await;
    let refused = start_setup(&api, &ada).await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.reauthentication_required");
    assert!(
        reads(&api, "SELECT id::text FROM totp_setups")
            .await
            .is_empty()
    );

    users::authenticated_ago(&api, &ada, 1).await;
    let response = start_setup(&api, &ada).await;
    assert_eq!(response.status, 200);
    let setup = response.json();
    let secret = setup["secret"].as_str().unwrap();
    assert_eq!(secret.len(), 32);
    // The product's name, as the URI encodes it: one that `just rename` gives may hold spaces
    // (ARCH-15).
    let issuer = "AntWorker".replace(' ', "%20");
    assert_eq!(
        setup["uri"],
        format!(
            "otpauth://totp/{issuer}:ada%40example.com?secret={secret}&issuer={issuer}\
             &algorithm=SHA1&digits=6&period=30"
        )
    );
    let lasts = reads(
        &api,
        "SELECT (expires_at - created_at)::text || ' ' || key_version FROM totp_setups",
    )
    .await;
    assert_eq!(lasts, ["00:15:00 1"]);
    let expires = reads(
        &api,
        "SELECT to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
         FROM totp_setups",
    )
    .await;
    assert_eq!(setup["expires_at"], expires[0].as_str());
    let id = reads(&api, "SELECT id::text FROM totp_setups")
        .await
        .remove(0);
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api, "second_factor.setup_started").await,
        [format!("{ada_id} {ada_id} second_factor {id}")]
    );

    // A setup replaces the one before it.
    let again = start_setup(&api, &ada).await.json();
    assert_ne!(again["secret"], setup["secret"]);
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM totp_setups").await,
        ["1"]
    );
}

#[tokio::test]
async fn auth_056_confirming_a_setup_enables_the_app_with_ten_recovery_codes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let secret = start_setup(&api, &ada).await.json()["secret"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = confirm(&api, &ada, &code(&api, &secret).await).await;

    assert_eq!(response.status, 200);
    let codes = response.json()["recovery_codes"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(codes.len(), 10);
    assert!(
        reads(&api, "SELECT id::text FROM totp_setups")
            .await
            .is_empty()
    );
    let app = reads(&api, "SELECT id::text FROM totp_factors")
        .await
        .remove(0);
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM recovery_codes").await,
        ["10"]
    );
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api, "second_factor.enabled").await,
        [format!("{ada_id} {ada_id} second_factor {app}")]
    );

    // A new setup's confirmation puts its app in the first one's place, with new codes.
    users::authenticated_ago(&api, &ada, 0).await;
    let next = start_setup(&api, &ada).await.json()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let replaced = confirm(&api, &ada, &code(&api, &next).await).await;
    assert_eq!(replaced.status, 200);
    let apps = reads(&api, "SELECT id::text FROM totp_factors").await;
    assert_eq!(apps.len(), 1);
    assert_ne!(apps[0], app);
    assert_ne!(replaced.json()["recovery_codes"][0], codes[0]);
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM recovery_codes").await,
        ["10"]
    );
}

#[tokio::test]
async fn auth_056_a_wrong_code_records_it_and_leaves_the_setup_and_none_answers_setup_expired() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let none = confirm(&api, &ada, "123456").await;
    assert_eq!(none.status, 422);
    assert_eq!(none.json()["code"], "auth.setup_expired");

    let secret = start_setup(&api, &ada).await.json()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let wrong = confirm(&api, &ada, &other_code(&code(&api, &secret).await)).await;
    assert_eq!(wrong.status, 422);
    assert_eq!(wrong.json()["code"], "auth.wrong_code");
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api, "second_factor.check_failed").await,
        [format!("{ada_id} {ada_id} user {ada_id}")]
    );
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM totp_setups").await,
        ["1"]
    );
    assert!(
        reads(&api, "SELECT id::text FROM totp_factors")
            .await
            .is_empty()
    );

    let invalid = api
        .post_raw(
            "/v1/users/me/second-factors/totp/setup/confirm",
            &json!({ "code": "12345" }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(invalid.status, 400);
    assert_eq!(invalid.json()["errors"][0]["field"], "/code");
    assert_eq!(invalid.json()["errors"][0]["code"], "invalid_code");

    // A setup whose 15 minutes passed confirms nothing.
    api.database()
        .owner_executes("UPDATE totp_setups SET expires_at = now() - interval '1 second'")
        .await;
    let expired = confirm(&api, &ada, &code(&api, &secret).await).await;
    assert_eq!(expired.status, 422);
    assert_eq!(expired.json()["code"], "auth.setup_expired");
}

#[tokio::test]
async fn auth_056_a_code_of_more_than_32_characters_answers_400_at_its_field() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let secret = start_setup(&api, &ada).await.json()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    // The right code, which the spaces before it take past 32 characters.
    let long = format!("{}{}", " ".repeat(27), code(&api, &secret).await);

    let refused = api
        .post_raw(
            "/v1/users/me/second-factors/totp/setup/confirm",
            &json!({ "code": long }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(refused.status, 400, "{}", refused.json());
    assert_eq!(refused.json()["errors"][0]["field"], "/code");
    assert_eq!(refused.json()["errors"][0]["code"], "invalid_code");
}

#[tokio::test]
async fn auth_057_the_step_that_a_check_accepted_works_no_more() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let secret = start_setup(&api, &ada).await.json()["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let used = code(&api, &secret).await;
    assert_eq!(confirm(&api, &ada, &used).await.status, 200);

    // The confirmation's code, in the same step, doesn't re-authenticate.
    let again = reauthenticate(&api, &ada, &json!({ "password": PASSWORD, "code": used })).await;

    assert_eq!(again.status, 403);
    assert_eq!(again.json()["code"], "auth.wrong_second_factor");
    let step = reads(
        &api,
        "SELECT (last_used_step = floor(extract(epoch FROM now()) / 30) \
         OR last_used_step = floor(extract(epoch FROM now()) / 30) - 1)::text FROM totp_factors",
    )
    .await;
    assert_eq!(step, ["true"]);
}

#[tokio::test]
async fn auth_062_a_recovery_code_works_once_in_place_of_the_app_s_and_only_its_sha_256_is_stored()
{
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let App { recovery_codes, .. } = add_app(&api, &ada).await;
    for code in &recovery_codes {
        assert_eq!(code.len(), 29, "{code}");
        assert_eq!(code.split('-').count(), 6, "{code}");
    }
    // Each code's hash is the SHA-256 of its user's ID and its characters, and holds no code.
    let plain = recovery_codes[0].replace('-', "");
    let stored = reads(&api, "SELECT encode(code_hash, 'hex') FROM recovery_codes").await;
    let expected = reads(
        &api,
        &format!(
            "SELECT encode(sha256(uuid_send('{}'::uuid) || convert_to('{plain}', 'UTF8')), 'hex')",
            ada.user_id
        ),
    )
    .await;
    assert!(stored.contains(&expected[0]), "{stored:?}");
    let raw = reads(
        &api,
        "SELECT encode(code_hash, 'escape') FROM recovery_codes",
    )
    .await;
    assert!(raw.iter().all(|hash| !hash.contains(&plain)));

    let before = reads(&api, "SELECT id::text FROM recovery_codes ORDER BY 1").await;
    // Lowercase, without its hyphens, it reads as the code.
    let typed = recovery_codes[0].replace('-', "").to_lowercase();
    let first = reauthenticate(
        &api,
        &ada,
        &json!({ "password": PASSWORD, "recovery_code": typed }),
    )
    .await;
    assert_eq!(first.status, 200, "{}", first.json());
    // With the tokens that the proof gave the session (AUTH-095).
    let ada = SignedIn::from(&first);
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM recovery_codes").await,
        ["9"]
    );
    let after = reads(&api, "SELECT id::text FROM recovery_codes ORDER BY 1").await;
    let used: Vec<&String> = before.iter().filter(|id| !after.contains(id)).collect();
    assert_eq!(used.len(), 1);
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api, "recovery_code.used").await,
        [format!("{ada_id} {ada_id} recovery_code {}", used[0])]
    );

    let second = reauthenticate(
        &api,
        &ada,
        &json!({ "password": PASSWORD, "recovery_code": recovery_codes[0] }),
    )
    .await;
    assert_eq!(second.status, 403);
    assert_eq!(second.json()["code"], "auth.wrong_second_factor");
    let invalid = api
        .post_raw(
            "/v1/users/me/sessions/current/reauthenticate",
            &json!({ "password": PASSWORD, "recovery_code": "ABCD-EFGH" }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(invalid.status, 400);
    assert_eq!(invalid.json()["errors"][0]["field"], "/recovery_code");
}

#[tokio::test]
async fn auth_063_new_recovery_codes_need_a_recent_proof_and_an_app_and_replace_the_old_ones() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let none = new_codes(&api, &ada).await;
    assert_eq!(none.status, 409);
    assert_eq!(none.json()["code"], "auth.no_second_factor");

    let app = add_app(&api, &ada).await;
    users::authenticated_ago(&api, &ada, 10).await;
    let refused = new_codes(&api, &ada).await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.reauthentication_required");

    users::authenticated_ago(&api, &ada, 0).await;
    let response = new_codes(&api, &ada).await;
    assert_eq!(response.status, 200);
    let codes = response.json()["recovery_codes"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(codes.len(), 10);
    assert!(!codes.contains(&json!(app.recovery_codes[0])));
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api, "recovery_codes.issued").await,
        [format!("{ada_id} {ada_id} user {ada_id}")]
    );
    let old = reauthenticate(
        &api,
        &ada,
        &json!({ "password": PASSWORD, "recovery_code": app.recovery_codes[0] }),
    )
    .await;
    assert_eq!(old.json()["code"], "auth.wrong_second_factor");
    let fresh = reauthenticate(
        &api,
        &ada,
        &json!({ "password": PASSWORD, "recovery_code": codes[0] }),
    )
    .await;
    assert_eq!(fresh.status, 200);
}

#[tokio::test]
async fn auth_064_removing_the_app_deletes_it_with_its_codes_and_challenges_once_proved() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let nothing = remove(&api, &ada).await;
    assert_eq!(nothing.status, 204);
    assert!(entries(&api, "second_factor.removed").await.is_empty());
    assert!(
        reads(
            &api,
            "SELECT id::text FROM jobs WHERE kind = 'platform.send_email'"
        )
        .await
        .is_empty()
    );

    add_app(&api, &ada).await;
    let challenge = second_factors::challenge(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 10).await;
    let refused = remove(&api, &ada).await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.reauthentication_required");

    users::authenticated_ago(&api, &ada, 0).await;
    let app = reads(&api, "SELECT id::text FROM totp_factors")
        .await
        .remove(0);
    let response = remove(&api, &ada).await;

    assert_eq!(response.status, 204);
    for table in ["totp_factors", "recovery_codes", "sign_in_challenges"] {
        assert!(
            reads(&api, &format!("SELECT id::text FROM {table}"))
                .await
                .is_empty(),
            "{table}"
        );
    }
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api, "second_factor.removed").await,
        [format!("{ada_id} {ada_id} second_factor {app}")]
    );
    // Her session stays, and a sign-in needs the password alone.
    assert_eq!(read(&api, &ada).await.status, 200);
    let completed = second_factors::complete(&api, &challenge, json!({ "code": "123456" })).await;
    assert_eq!(completed.json()["code"], "auth.invalid_challenge");
    assert_eq!(
        users::sign_in(&api, "ada@example.com", PASSWORD)
            .await
            .status,
        201
    );
}

#[tokio::test]
async fn auth_065_both_codes_or_a_malformed_one_answer_400_at_its_field() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let padded = format!("424242{}", " ".repeat(27));
    let bodies = [
        json!({ "password": PASSWORD, "code": "12345" }),
        json!({ "password": PASSWORD, "code": padded }),
        json!({ "password": PASSWORD, "code": "424242", "recovery_code": app.recovery_codes[0] }),
        json!({ "password": PASSWORD, "recovery_code": "ABCD-EFGH" }),
    ];

    let mut refused = Vec::new();
    for body in &bodies {
        let answer = api
            .post_raw("/v1/users/me/sessions/current/reauthenticate", body)
            .bearer(&ada.access)
            .send()
            .await;
        assert_eq!(answer.status, 400, "{body}");
        refused.push(answer.json()["errors"][0]["field"].clone());
    }

    assert_eq!(
        refused,
        ["/code", "/code", "/code", "/recovery_code"].map(Value::from)
    );
}

#[tokio::test]
async fn auth_065_with_an_app_re_authenticating_needs_its_code_beside_the_password() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    users::authenticated_ago(&api, &ada, 10).await;

    let missing = reauthenticate(&api, &ada, &json!({ "password": PASSWORD })).await;
    assert_eq!(missing.status, 403);
    assert_eq!(missing.json()["code"], "auth.second_factor_required");

    let code_now = code(&api, &app.secret).await;
    let wrong = reauthenticate(
        &api,
        &ada,
        &json!({ "password": PASSWORD, "code": other_code(&code_now) }),
    )
    .await;
    assert_eq!(wrong.status, 403);
    assert_eq!(wrong.json()["code"], "auth.wrong_second_factor");
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api, "second_factor.check_failed").await,
        [format!("{ada_id} {ada_id} user {ada_id}")]
    );

    // A wrong password answers as it does without an app, whatever the code.
    let wrong_password = reauthenticate(
        &api,
        &ada,
        &json!({ "password": "wrong-password-123", "code": code_now }),
    )
    .await;
    assert_eq!(wrong_password.json()["code"], "auth.wrong_password");

    let both = api
        .post_raw(
            "/v1/users/me/sessions/current/reauthenticate",
            &json!({ "password": PASSWORD, "code": code_now,
                     "recovery_code": app.recovery_codes[0] }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(both.status, 400);
    assert_eq!(both.json()["errors"][0]["field"], "/code");

    let right = reauthenticate(
        &api,
        &ada,
        &json!({ "password": PASSWORD, "code": code_now }),
    )
    .await;
    assert_eq!(right.status, 200, "{}", right.json());
    // The session proved who its user is, so a sensitive action may follow, with the tokens
    // that the proof gave it (AUTH-095).
    assert_eq!(start_setup(&api, &SignedIn::from(&right)).await.status, 200);
}

#[tokio::test]
async fn auth_065_with_an_app_changing_the_password_needs_its_code_beside_the_current_one() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let change = |body: Value| {
        let api = api.clone();
        let access = ada.access.clone();
        async move {
            api.post("/v1/users/me/password/change", &body)
                .bearer(&access)
                .send()
                .await
        }
    };
    let new_password = "violet-harbor-73-brisk";

    let missing = change(json!({ "current_password": PASSWORD, "password": new_password })).await;
    assert_eq!(missing.status, 403);
    assert_eq!(missing.json()["code"], "auth.second_factor_required");
    let code_now = code(&api, &app.secret).await;
    let wrong = change(json!({
        "current_password": PASSWORD, "password": new_password, "code": other_code(&code_now)
    }))
    .await;
    assert_eq!(wrong.json()["code"], "auth.wrong_second_factor");
    assert_eq!(
        users::sign_in(&api, "ada@example.com", new_password)
            .await
            .status,
        401
    );

    let right = change(json!({
        "current_password": PASSWORD, "password": new_password, "code": code_now
    }))
    .await;
    assert_eq!(right.status, 200, "{}", right.json());
    assert_eq!(
        users::sign_in(&api, "ada@example.com", new_password)
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn auth_068_each_change_of_the_second_factor_and_each_recovery_code_used_tells_the_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let app = add_app(&api, &ada).await;
    users::authenticated_ago(&api, &ada, 0).await;
    assert_eq!(new_codes(&api, &ada).await.status, 200);
    let codes = new_codes(&api, &ada).await.json()["recovery_codes"].clone();
    forget_steps(&api, &ada.user_id).await;
    let used = reauthenticate(
        &api,
        &ada,
        &json!({ "password": PASSWORD, "recovery_code": codes[0] }),
    )
    .await;
    assert_eq!(used.status, 200);
    // With the tokens that the proof gave the session (AUTH-095).
    let ada = SignedIn::from(&used);
    assert_eq!(remove(&api, &ada).await.status, 204);
    api.run_jobs().await;

    let subjects: Vec<String> = mailbox::to(&address)
        .await
        .into_iter()
        .map(|email| email.subject)
        .filter(|subject| !subject.contains("verification code"))
        .collect();
    assert_eq!(
        subjects,
        [
            "The authenticator app of your AntWorker account was removed",
            "A recovery code was used on your AntWorker account",
            "New recovery codes for your AntWorker account",
            "New recovery codes for your AntWorker account",
            "An authenticator app now signs in to your AntWorker account",
        ]
    );
    let emails = mailbox::to(&address).await;
    let recovery = emails
        .iter()
        .find(|email| email.subject.starts_with("A recovery code"))
        .unwrap();
    assert!(
        recovery.text.contains("9 recovery codes are left."),
        "{}",
        recovery.text
    );
    for email in &emails {
        assert!(!email.text.contains(&app.secret));
        assert!(!email.text.contains("http"), "{}", email.text);
    }
}

#[tokio::test]
async fn auth_068_with_email_off_no_change_of_the_second_factor_enqueues_an_email() {
    let api = Api::without_email().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    users::authenticated_ago(&api, &ada, 0).await;
    assert_eq!(remove(&api, &ada).await.status, 204);
    assert!(
        reads(
            &api,
            "SELECT id::text FROM jobs WHERE kind = 'platform.send_email'"
        )
        .await
        .is_empty()
    );
}

#[tokio::test]
async fn auth_055_a_setup_whose_session_ended_once_it_was_authenticated_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // Her first authenticated request adds her bucket of requests (PLATFORM-054).
    let bucket =
        users::added_bucket(&api, api.get("/v1/users/me").bearer(&ada.access).send()).await;

    let (status, body) = users::after_its_session_ended(
        &api,
        &ada,
        &bucket,
        (http::Method::POST, "/v1/users/me/second-factors/totp/setup"),
        None,
    )
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert!(
        reads(&api, "SELECT id::text FROM totp_setups")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_063_new_codes_whose_session_ended_once_they_were_authenticated_answer_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    // A first request adds the bucket that new codes and removals share (AUTH-063).
    let bucket = users::added_bucket(&api, new_codes(&api, &ada)).await;
    let codes = reads(&api, "SELECT id::text FROM recovery_codes ORDER BY 1").await;

    let (status, body) = users::after_its_session_ended(
        &api,
        &ada,
        &bucket,
        (
            http::Method::POST,
            "/v1/users/me/second-factors/recovery-codes",
        ),
        None,
    )
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert_eq!(
        reads(&api, "SELECT id::text FROM recovery_codes ORDER BY 1").await,
        codes
    );
}

#[tokio::test]
async fn auth_064_a_removal_whose_session_ended_once_it_was_authenticated_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    let bucket = users::added_bucket(&api, new_codes(&api, &ada)).await;

    let (status, body) = users::after_its_session_ended(
        &api,
        &ada,
        &bucket,
        (http::Method::DELETE, "/v1/users/me/second-factors/totp"),
        None,
    )
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM totp_factors").await,
        ["1"]
    );
}

#[tokio::test]
async fn auth_063_new_codes_and_removals_of_the_app_count_in_one_bucket_of_the_user_s() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    for _ in 0..10 {
        assert_eq!(new_codes(&api, &ada).await.status, 200);
    }

    let limited = remove(&api, &ada).await;

    assert_eq!(limited.status, 429);
    assert_eq!(limited.json()["code"], "request.rate_limited");
    assert!(
        limited
            .header("ratelimit-policy")
            .starts_with("\"second_factor_change\";q=10;w=86400"),
        "{}",
        limited.header("ratelimit-policy")
    );
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM totp_factors").await,
        ["1"]
    );
    assert_eq!(new_codes(&api, &ada).await.status, 429);
}

#[tokio::test]
async fn auth_062_a_recovery_code_outlives_a_change_of_hmac_key() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    // A replica with another HMAC_KEY, 32 bytes of 8, as after the key's change.
    let other = api
        .instance(
            crate::support::Options::default()
                .set("HMAC_KEY", "CAgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAg="),
        )
        .await;

    let response = reauthenticate(
        &other,
        &ada,
        &json!({ "password": PASSWORD, "recovery_code": app.recovery_codes[0] }),
    )
    .await;

    assert_eq!(response.status, 200, "{}", response.json());
}

/// A route that checks a second factor beside another proof, apart from a signed-in user's
/// re-authentication with the password: a completion of a sign-in, a reset's confirmation, a
/// re-authentication through a provider and a change of the password.
#[derive(Clone, Copy, Debug)]
enum Proof {
    Completion,
    Reset,
    Provider,
    Change,
}

impl Proof {
    /// Each of them.
    const ALL: [Self; 4] = [Self::Completion, Self::Reset, Self::Provider, Self::Change];

    /// The field of the route's body that carries an app's code.
    fn code_field(self) -> &'static str {
        match self {
            Self::Completion | Self::Change => "code",
            Self::Reset | Self::Provider => "second_factor_code",
        }
    }

    /// What the route answers a second factor that isn't the user's: its status and its code.
    fn refusal(self) -> (u16, &'static str) {
        match self {
            Self::Completion | Self::Reset => (401, "auth.invalid_second_factor"),
            Self::Provider | Self::Change => (403, "auth.wrong_second_factor"),
        }
    }
}

/// Ada, with an app, who sends a [`Proof`]'s route her second factors.
struct Prover {
    proof: Proof,
    api: Api,
    /// The stand-in provider that the API signs in through, which answers while it lives.
    _upstream: Upstream,
    address: String,
    user: SignedIn,
    app: App,
}

impl Prover {
    /// Ada, at an address of her own, on an API with email on or off as `email_on` says, who
    /// signed in through the stand-in for a re-authentication through it, and registered with
    /// [`PASSWORD`] for any other route, then set up an app.
    async fn arrange(proof: Proof, email_on: bool) -> Self {
        let upstream = Upstream::start().await;
        let mut options = providers::stand_in(&upstream);
        if !email_on {
            options = options.set("EMAIL", "off");
        }
        let api = Api::with(options).await;
        let address = mailbox::address("ada");
        let user = match proof {
            Proof::Provider => {
                gives(&upstream, identity("subject-1", &address, true));
                providers::signed_in(&api).await
            }
            Proof::Completion | Proof::Reset | Proof::Change => {
                users::register(&api, &address).await
            }
        };
        let app = add_app(&api, &user).await;
        Self {
            proof,
            api,
            _upstream: upstream,
            address,
            user,
            app,
        }
    }

    /// Sends the route `factor`, the fields of a second factor, beside the route's other proof:
    /// a sign-in's challenge, a reset code that the API sent the address, the stand-in's code, or
    /// the current password, which a change keeps. Keeps the tokens that a proof of Ada's
    /// session gives it (AUTH-095).
    async fn send(&mut self, factor: Value) -> Response {
        let api = &self.api;
        let response = match self.proof {
            Proof::Completion => {
                let challenge = second_factors::challenge(api, &self.address).await;
                second_factors::complete(api, &challenge, factor).await
            }
            Proof::Reset => {
                let asked = api
                    .post("/v1/password-resets", &json!({ "email": self.address }))
                    .send()
                    .await;
                assert_eq!(asked.status, 202, "{}", asked.json());
                api.run_jobs().await;
                let emails = mailbox::to(&self.address).await;
                let email = emails
                    .iter()
                    .find(|email| email.subject.contains("password reset code"))
                    .expect("an email with a reset code");
                let code = mailbox::grouped_code_in(&email.text).expect("a reset code");
                let body = json!({ "email": self.address, "code": code, "password": PASSWORD });
                api.post("/v1/password-resets/confirm", &with(body, factor))
                    .send()
                    .await
            }
            Proof::Provider => {
                api.post(
                    "/v1/users/me/sessions/current/reauthenticate/provider",
                    &with(providers::grant(), factor),
                )
                .bearer(&self.user.access)
                .send()
                .await
            }
            Proof::Change => {
                let body = json!({ "current_password": PASSWORD, "password": PASSWORD });
                api.post("/v1/users/me/password/change", &with(body, factor))
                    .bearer(&self.user.access)
                    .send()
                    .await
            }
        };
        if matches!(self.proof, Proof::Provider | Proof::Change) && response.status == 200 {
            self.user = SignedIn::from(&response);
        }
        response
    }

    /// The fields of the app's code that it shows now.
    async fn app_code(&self) -> Value {
        let code = code(&self.api, &self.app.secret).await;
        json!({ self.proof.code_field(): code })
    }
}

/// `body` with the fields of `extra` added.
fn with(mut body: Value, extra: Value) -> Value {
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body
}

#[tokio::test]
async fn auth_057_each_route_that_checks_an_app_accepts_its_code_once_and_keeps_its_step() {
    for proof in Proof::ALL {
        let mut prover = Prover::arrange(proof, true).await;
        let factor = prover.app_code().await;

        let accepted = prover.send(factor.clone()).await;
        let again = prover.send(factor).await;

        assert!(
            accepted.status.is_success(),
            "{proof:?}: {}",
            accepted.json()
        );
        assert_eq!(
            (
                again.status.as_u16(),
                again.json()["code"].as_str().unwrap()
            ),
            proof.refusal(),
            "{proof:?}: the step that a check accepted works no more"
        );
        let step = reads(
            &prover.api,
            "SELECT (last_used_step = floor(extract(epoch FROM now()) / 30) \
             OR last_used_step = floor(extract(epoch FROM now()) / 30) - 1)::text \
             FROM totp_factors",
        )
        .await;
        assert_eq!(
            step,
            ["true"],
            "{proof:?}: the accepted step is the last used one"
        );
    }
}

#[tokio::test]
async fn auth_062_a_recovery_code_works_once_in_place_of_the_app_s_on_each_route_that_checks_one() {
    for proof in Proof::ALL {
        let mut prover = Prover::arrange(proof, true).await;
        let user_id = prover.user.user_id.clone();
        let before = reads(
            &prover.api,
            "SELECT id::text FROM recovery_codes ORDER BY 1",
        )
        .await;
        let code = prover.app.recovery_codes[0].clone();
        // Lowercase, without its hyphens, it reads as the code.
        let typed = code.replace('-', "").to_lowercase();

        let used = prover.send(json!({ "recovery_code": typed })).await;
        let again = prover.send(json!({ "recovery_code": code })).await;

        assert!(used.status.is_success(), "{proof:?}: {}", used.json());
        let after = reads(
            &prover.api,
            "SELECT id::text FROM recovery_codes ORDER BY 1",
        )
        .await;
        let gone: Vec<&String> = before.iter().filter(|id| !after.contains(id)).collect();
        assert_eq!(gone.len(), 1, "{proof:?}: using it deletes it");
        assert_eq!(after.len(), 9, "{proof:?}");
        let recorded = reads(
            &prover.api,
            "SELECT user_id || ' ' || resource_type || ' ' || resource_id FROM audit_entries \
             WHERE action = 'recovery_code.used'",
        )
        .await;
        assert_eq!(
            recorded,
            [format!("{user_id} recovery_code {}", gone[0])],
            "{proof:?}"
        );
        assert_eq!(
            (
                again.status.as_u16(),
                again.json()["code"].as_str().unwrap()
            ),
            proof.refusal(),
            "{proof:?}: it works once"
        );
    }
}

#[tokio::test]
async fn auth_068_a_recovery_code_used_on_each_route_that_checks_one_tells_the_address() {
    for proof in Proof::ALL {
        let mut prover = Prover::arrange(proof, true).await;

        let used = prover
            .send(json!({ "recovery_code": prover.app.recovery_codes[0] }))
            .await;

        assert!(used.status.is_success(), "{proof:?}: {}", used.json());
        prover.api.run_jobs().await;
        let emails = mailbox::to(&prover.address).await;
        let told: Vec<_> = emails
            .iter()
            .filter(|email| email.subject == "A recovery code was used on your AntWorker account")
            .collect();
        assert_eq!(told.len(), 1, "{proof:?}");
        assert!(
            told[0].text.contains("9 recovery codes are left."),
            "{proof:?}: {}",
            told[0].text
        );
        assert!(!told[0].text.contains(&prover.app.secret), "{proof:?}");
        assert!(
            prover
                .app
                .recovery_codes
                .iter()
                .all(|code| !told[0].text.contains(code.as_str())),
            "{proof:?}"
        );
        assert!(
            !told[0].text.contains("http"),
            "{proof:?}: {}",
            told[0].text
        );
    }
    // With email off, none is enqueued; a reset's confirmation answers 503 then (AUTH-038).
    for proof in [Proof::Completion, Proof::Provider, Proof::Change] {
        let mut prover = Prover::arrange(proof, false).await;

        let used = prover
            .send(json!({ "recovery_code": prover.app.recovery_codes[0] }))
            .await;

        assert!(used.status.is_success(), "{proof:?}: {}", used.json());
        assert!(
            reads(
                &prover.api,
                "SELECT id::text FROM jobs WHERE kind = 'platform.send_email'"
            )
            .await
            .is_empty(),
            "{proof:?}"
        );
    }
}
