//! A user's second factors: reading them, setting up an authenticator app, recovery codes,
//! removing the app, the codes that a signed-in user's re-authentication and change of the
//! password ask for, and the emails that tell the address (the auth spec's "Second factors").

use serde_json::{Value, json};

use crate::support::{
    Api,
    api::Response,
    mailbox,
    second_factors::{self, App, add_app, code, confirm, forget_steps, other_code, start_setup},
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

/// The first column of each row of `sql`, as the `owner` reads it.
async fn reads(api: &Api, sql: &str) -> Vec<String> {
    api.database().owner_reads(sql).await
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
async fn auth_055_a_setup_needs_a_recent_proof_and_answers_the_secret_once() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 10).await;
    let refused = start_setup(&api, &ada).await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.reauthentication_required");
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
    assert_eq!(
        setup["uri"],
        format!(
            "otpauth://totp/AntWorker:ada%40example.com?secret={secret}&issuer=AntWorker\
             &algorithm=SHA1&digits=6&period=30"
        )
    );
    let lasts = reads(
        &api,
        "SELECT (expires_at - created_at)::text || ' ' || key_version FROM totp_setups",
    )
    .await;
    assert_eq!(lasts, ["00:15:00 1"]);
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
async fn auth_062_a_recovery_code_works_once_in_place_of_the_app_s_and_only_its_hmac_is_stored() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let App { recovery_codes, .. } = add_app(&api, &ada).await;
    for code in &recovery_codes {
        assert_eq!(code.len(), 29, "{code}");
        assert_eq!(code.split('-').count(), 6, "{code}");
    }
    let stored = reads(
        &api,
        "SELECT encode(code_hash, 'escape') FROM recovery_codes",
    )
    .await;
    let plain = recovery_codes[0].replace('-', "");
    assert!(stored.iter().all(|hash| !hash.contains(&plain)));

    // Lowercase, without its hyphens, it reads as the code.
    let typed = recovery_codes[0].replace('-', "").to_lowercase();
    let first = reauthenticate(
        &api,
        &ada,
        &json!({ "password": PASSWORD, "recovery_code": typed }),
    )
    .await;
    assert_eq!(first.status, 200, "{}", first.json());
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM recovery_codes").await,
        ["9"]
    );
    let code_ids = entries(&api, "recovery_code.used").await;
    assert_eq!(code_ids.len(), 1);
    assert!(code_ids[0].contains(" recovery_code "), "{}", code_ids[0]);

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
    assert_eq!(refused.json()["code"], "auth.reauthentication_required");

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

    add_app(&api, &ada).await;
    let challenge = second_factors::challenge(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 10).await;
    let refused = remove(&api, &ada).await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.reauthentication_required");

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
    // The session proved who its user is, so a sensitive action may follow.
    assert_eq!(start_setup(&api, &ada).await.status, 200);
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
    assert_eq!(right.status, 204, "{}", right.json());
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
