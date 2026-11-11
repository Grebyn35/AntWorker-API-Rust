//! Resetting a forgotten password with a code sent to the account's address, and changing the
//! password with the current one (the auth spec's "Password reset" and "Changing the password",
//! AUTH-034 to AUTH-041).

use serde_json::{Value, json};

use crate::support::{
    Api, Options,
    api::Response,
    mailbox,
    users::{self, SignedIn},
};

/// The new password of the tests' resets and changes.
const NEW_PASSWORD: &str = "lantern-orchard-58-brisk";

/// Asks for a reset of `email`'s password.
async fn ask(api: &Api, email: &str) -> Response {
    api.post("/v1/password-resets", &json!({ "email": email }))
        .send()
        .await
}

/// Confirms a reset of `email`'s password with `code`, to [`NEW_PASSWORD`].
async fn confirm(api: &Api, email: &str, code: &str) -> Response {
    api.post(
        "/v1/password-resets/confirm",
        &json!({ "email": email, "code": code, "password": NEW_PASSWORD }),
    )
    .send()
    .await
}

/// Changes `user`'s password from `current` to [`NEW_PASSWORD`].
async fn change(api: &Api, user: &SignedIn, current: &str) -> Response {
    api.post(
        "/v1/users/me/password/change",
        &json!({ "current_password": current, "password": NEW_PASSWORD }),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// The reset code in the newest reset email that `address` received, once the API's jobs have
/// run.
async fn code_sent(api: &Api, address: &str) -> String {
    api.run_jobs().await;
    let emails = mailbox::to(address).await;
    let email = emails
        .iter()
        .find(|email| email.subject.contains("password reset code"))
        .expect("an email with a reset code");
    mailbox::grouped_code_in(&email.text).expect("a reset code in the email")
}

/// The entries of `user_id`'s log, the oldest first: each one's action, actor, resource type,
/// resource ID and details, which SEC-18's check asks each action's test to assert.
async fn log(api: &Api, user_id: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT action || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_type \
             || ' ' || coalesce(resource_id::text, '-') || ' ' || details::text \
             FROM audit_entries WHERE user_id = '{user_id}' ORDER BY id"
        ))
        .await
}

/// The ID of `user_id`'s password identity.
async fn identity(api: &Api, user_id: &str) -> String {
    api.database()
        .owner_reads(&format!(
            "SELECT id::text FROM identities WHERE user_id = '{user_id}'"
        ))
        .await
        .remove(0)
}

/// `body` without its trace ID, which differs between any two responses.
fn without_trace(mut body: Value) -> Value {
    body.as_object_mut().unwrap().remove("trace_id");
    body
}

#[tokio::test]
async fn auth_034_asking_enqueues_the_reset_code_s_job_for_the_account_and_answers_202() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;

    let asked = ask(&api, &address).await;

    assert_eq!(asked.status, 202);
    assert!(asked.body.is_empty());
    assert_eq!(
        api.database().owner_reads("SELECT kind || ' ' || coalesce(user_id::text, '-') || ' ' || payload::text FROM jobs"
        )
        .await,
        [format!(
            "auth.send_verification_code {} {{\"purpose\": \"password_reset\"}}",
            ada.user_id
        )]
    );
    assert_eq!(code_sent(&api, &address).await.len(), 19);
}

#[tokio::test]
async fn auth_034_an_address_that_no_account_holds_gets_the_same_answer_and_the_same_job_for_no_one()
 {
    let api = Api::new().await;
    let address = mailbox::address("nobody");

    let asked = ask(&api, &address).await;

    assert_eq!(asked.status, 202);
    assert!(asked.body.is_empty());
    assert_eq!(
        api.database().owner_reads("SELECT kind || ' ' || coalesce(user_id::text, '-') || ' ' || payload::text FROM jobs"
        )
        .await,
        ["auth.send_verification_code - {\"purpose\": \"password_reset\"}"]
    );
}

#[tokio::test]
async fn auth_034_an_account_without_a_password_gets_a_reset_code() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::without_password(api.database(), &address).await;

    assert_eq!(ask(&api, &address).await.status, 202);

    assert_eq!(code_sent(&api, &address).await.len(), 19);
}

#[tokio::test]
async fn auth_034_a_request_whose_email_isn_t_an_address_answers_400_at_email() {
    let api = Api::new().await;

    let refused = api
        .post_raw("/v1/password-resets", &json!({ "email": "ada" }))
        .send()
        .await;

    assert_eq!(refused.status, 400);
    let error = &refused.json()["errors"][0];
    assert_eq!(error["field"], "/email");
    assert_eq!(error["code"], "invalid_email");
    assert_eq!(refused.json()["errors"].as_array().unwrap().len(), 1);
    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_034_with_email_off_asking_answers_503_once_its_body_converts() {
    let api = Api::without_email().await;
    users::register(&api, "ada@example.com").await;

    let asked = ask(&api, "ada@example.com").await;
    let malformed = api
        .post_raw("/v1/password-resets", &json!({ "email": "not an address" }))
        .send()
        .await;

    assert_eq!(asked.status, 503);
    assert_eq!(asked.json()["code"], "request.feature_disabled");
    assert_eq!(malformed.status, 400);
    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_035_asking_counts_in_a_bucket_of_the_address_before_it_enqueues_anything() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register(&api, &address).await;

    let mut statuses = Vec::new();
    for _ in 0..3 {
        statuses.push(ask(&api, &address).await.status);
    }
    let refused = ask(&api, &address.to_uppercase()).await;
    let other = ask(&api, &mailbox::address("grace")).await;

    assert_eq!(statuses, [202, 202, 202]);
    assert_eq!(refused.status, 429);
    assert_eq!(refused.json()["code"], "request.rate_limited");
    assert!(
        refused
            .header("ratelimit-policy")
            .starts_with("\"reset_email\";q=3;w=600")
    );
    assert_eq!(other.status, 202);
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM jobs")
            .await,
        ["4"],
        "the refused request enqueued nothing"
    );
}

#[tokio::test]
async fn auth_035_requests_for_resets_fill_neither_bucket_of_codes_that_prove_an_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    for _ in 0..3 {
        assert_eq!(ask(&api, &address).await.status, 202);
    }

    let asked = api
        .request(http::Method::POST, "/v1/users/me/email/verification")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(asked.status, 202);
}

#[tokio::test]
async fn auth_035_requests_for_resets_and_registrations_of_an_address_count_apart() {
    let api = Api::new().await;
    let registered = mailbox::address("ada");
    let fresh = mailbox::address("grace");
    // The first registers; the next two count too, and answer that the address is in use.
    users::register(&api, &registered).await;
    for _ in 0..2 {
        let again = api
            .post("/v1/users", &users::registration(&registered))
            .send()
            .await;
        assert_eq!(again.status, 409);
    }
    for _ in 0..3 {
        assert_eq!(ask(&api, &fresh).await.status, 202);
    }

    let asked = ask(&api, &registered).await;
    let registration = api
        .post("/v1/users", &users::registration(&fresh))
        .send()
        .await;

    assert_eq!(asked.status, 202, "registrations fill no bucket of resets");
    assert_eq!(
        registration.status, 201,
        "resets fill no bucket of registrations"
    );
}

#[tokio::test]
async fn auth_036_the_job_stores_only_the_reset_code_s_hmac_and_sends_the_code() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    ask(&api, &address).await;

    let code = code_sent(&api, &address).await;

    let stored = api
        .database()
        .owner_reads(
            "SELECT purpose || ' ' || encode(code_hash, 'hex') || ' ' \
         || (expires_at - created_at)::text FROM verification_codes",
        )
        .await;
    assert_eq!(stored.len(), 1);
    let fields: Vec<&str> = stored[0].split(' ').collect();
    assert_eq!(fields[0], "password_reset");
    assert_eq!(fields[1].len(), 64);
    assert!(!fields[1].contains(&code.replace('-', "").to_lowercase()));
    assert_eq!(fields[2], "00:10:00");
    let id = api
        .database()
        .owner_reads("SELECT id::text FROM verification_codes")
        .await
        .remove(0);
    assert_eq!(
        log(&api, &ada.user_id).await.last().unwrap(),
        &format!("password_reset_code.issued - verification_code {id} {{}}")
    );
    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs")
            .await
            .is_empty()
    );
    let email = &mailbox::to(&address).await[0];
    assert_eq!(email.subject, "Your AntWorker password reset code");
}

#[tokio::test]
async fn auth_036_a_newer_reset_code_leaves_the_one_before_it_working() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register(&api, &address).await;
    ask(&api, &address).await;
    let first = code_sent(&api, &address).await;
    ask(&api, &address).await;
    let second = code_sent(&api, &address).await;

    assert_ne!(first, second);
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM verification_codes")
            .await,
        ["2"]
    );
    assert_eq!(confirm(&api, &address, &first).await.status, 204);
    // The reset deleted every reset code of the account.
    assert_eq!(confirm(&api, &address, &second).await.status, 401);
}

#[tokio::test]
async fn auth_036_issuing_a_reset_code_deletes_the_account_s_codes_that_expired() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register(&api, &address).await;
    ask(&api, &address).await;
    api.run_jobs().await;
    ask(&api, &address).await;
    api.run_jobs().await;
    // The first code's 10 minutes pass.
    let first = api
        .database()
        .owner_reads("SELECT id::text FROM verification_codes ORDER BY id LIMIT 1")
        .await
        .remove(0);
    api.database()
        .owner_executes(&format!(
            "UPDATE verification_codes SET expires_at = now() - interval '1 second' \
             WHERE id = '{first}'"
        ))
        .await;

    ask(&api, &address).await;
    api.run_jobs().await;

    let left = api
        .database()
        .owner_reads("SELECT id::text FROM verification_codes")
        .await;
    assert_eq!(left.len(), 2);
    assert!(!left.contains(&first));
}

#[tokio::test]
async fn auth_036_a_run_for_no_one_issues_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("nobody");
    ask(&api, &address).await;

    assert_eq!(api.run_jobs().await, 1);

    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM verification_codes")
            .await
            .is_empty()
    );
    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM audit_entries")
            .await
            .is_empty()
    );
    assert!(mailbox::to(&address).await.is_empty());
}

#[tokio::test]
async fn auth_036_an_attempt_after_a_failed_send_issues_a_code_of_its_own_beside_the_first() {
    let address = mailbox::address("ada");
    let unreachable = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let failing = Api::with(Options::default().set("SMTP_PORT", unreachable.port())).await;
    users::register(&failing, &address).await;
    ask(&failing, &address).await;
    assert_eq!(failing.run_jobs().await, 1);
    assert_eq!(
        failing
            .database()
            .owner_reads("SELECT state::text || ' ' || attempts FROM jobs")
            .await,
        ["waiting 1"]
    );
    // The next attempt, on a replica that reaches the mail server.
    failing
        .database()
        .owner_executes("UPDATE jobs SET run_at = now()")
        .await;
    let reaching = failing.instance(Options::default()).await;

    assert_eq!(reaching.run_jobs().await, 1);

    assert_eq!(
        failing
            .database()
            .owner_reads(
                "SELECT count(*)::text || ' ' || (SELECT count(*) FROM audit_entries \
                 WHERE action = 'password_reset_code.issued') FROM verification_codes"
            )
            .await,
        ["2 2"]
    );
    assert_eq!(mailbox::to(&address).await.len(), 1);
    let code = code_sent(&reaching, &address).await;
    assert_eq!(confirm(&reaching, &address, &code).await.status, 204);
}

#[tokio::test]
async fn auth_036_a_reset_code_leaves_the_account_s_verification_code_in_place() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    ask(&api, &address).await;

    assert_eq!(api.run_jobs().await, 2);

    assert_eq!(
        api.database()
            .owner_reads("SELECT purpose::text FROM verification_codes ORDER BY purpose")
            .await,
        ["email_verification", "password_reset"]
    );
    let verification = mailbox::to(&address)
        .await
        .into_iter()
        .find(|email| email.subject.contains("verification code"))
        .unwrap();
    let digits = verification
        .text
        .split_whitespace()
        .find(|word| word.len() == 6 && word.bytes().all(|byte| byte.is_ascii_digit()))
        .unwrap()
        .to_owned();
    let confirmed = api
        .post(
            "/v1/users/me/email/verification/confirm",
            &json!({ "code": digits }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(confirmed.status, 200);
}

#[tokio::test]
async fn auth_036_a_run_for_an_account_that_is_gone_issues_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    ask(&api, &address).await;
    api.database()
        .owner_executes(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    assert_eq!(api.run_jobs().await, 1);

    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM verification_codes")
            .await
            .is_empty()
    );
    assert!(mailbox::to(&address).await.is_empty());
}

#[tokio::test]
async fn auth_036_a_run_on_a_replica_with_email_off_issues_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register(&api, &address).await;
    ask(&api, &address).await;
    let off = api.instance(Options::email_off()).await;

    assert_eq!(off.run_jobs().await, 1);

    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM verification_codes")
            .await
            .is_empty()
    );
    assert!(mailbox::to(&address).await.is_empty());
}

#[tokio::test]
async fn auth_037_the_code_resets_the_password_ends_every_session_and_starts_none() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let other = users::new_session(&api, &address).await;
    ask(&api, &address).await;
    let code = code_sent(&api, &address).await;
    let mut signals = api.database().signals().await;

    let reset = confirm(&api, &address, &code.to_lowercase().replace('-', " ")).await;

    assert_eq!(reset.status, 204);
    assert!(reset.body.is_empty());
    for session in [&ada, &other] {
        let refused = api.get("/v1/users/me").bearer(&session.access).send().await;
        assert_eq!(refused.status, 401);
    }
    assert_eq!(
        users::sign_in(&api, &address, users::PASSWORD).await.status,
        401
    );
    let received: Vec<Value> = signals.received().await;
    assert_eq!(received.len(), 2);
    assert!(
        received
            .iter()
            .all(|signal| signal["kind"] == "session_ended")
    );
    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM verification_codes")
            .await
            .is_empty()
    );
    // It starts no session.
    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM sessions")
            .await
            .is_empty()
    );
    let entries = log(&api, &ada.user_id).await;
    let mut reset_entries: Vec<String> = entries
        .into_iter()
        .skip_while(|entry| !entry.starts_with("password_reset_code.issued"))
        .skip(1)
        .take(3)
        .collect();
    let reset = reset_entries.pop().unwrap();
    reset_entries.sort();
    let mut ended = [
        format!("session.ended - session {} {{}}", ada.session_id),
        format!("session.ended - session {} {{}}", other.session_id),
    ];
    ended.sort();
    assert_eq!(reset_entries, ended);
    assert_eq!(
        reset,
        format!(
            "password.reset - identity {} {{}}",
            identity(&api, &ada.user_id).await
        )
    );
    let signed_in = users::sign_in(&api, &address, NEW_PASSWORD).await;
    assert_eq!(signed_in.status, 201);
}

#[tokio::test]
async fn auth_037_a_reset_proves_the_address_of_an_account_that_hadn_t() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    ask(&api, &address).await;
    let code = code_sent(&api, &address).await;

    assert_eq!(confirm(&api, &address, &code).await.status, 204);

    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT email_verified::text FROM users WHERE id = '{}'",
                ada.user_id
            ))
            .await,
        ["true"]
    );
    let entries = log(&api, &ada.user_id).await;
    assert!(entries.contains(&format!("email.verified - user {} {{}}", ada.user_id)));
    let signed_in = users::sign_in(&api, &address, NEW_PASSWORD).await;
    assert_eq!(signed_in.json()["email_verification_required"], false);
}

#[tokio::test]
async fn auth_037_a_reset_sets_the_first_password_of_an_account_without_one() {
    let api = Api::new().await;
    let address = mailbox::address("owner");
    users::without_password(api.database(), &address).await;
    ask(&api, &address).await;
    let code = code_sent(&api, &address).await;

    assert_eq!(confirm(&api, &address, &code).await.status, 204);

    assert_eq!(
        api.database()
            .owner_reads("SELECT method::text FROM identities")
            .await,
        ["password"]
    );
    assert_eq!(
        users::sign_in(&api, &address, NEW_PASSWORD).await.status,
        201
    );
}

#[tokio::test]
async fn auth_037_a_reset_code_resets_the_password_once() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register(&api, &address).await;
    ask(&api, &address).await;
    let code = code_sent(&api, &address).await;
    assert_eq!(confirm(&api, &address, &code).await.status, 204);

    let again = confirm(&api, &address, &code).await;

    assert_eq!(again.status, 401);
    assert_eq!(again.json()["code"], "auth.invalid_reset_code");
}

#[tokio::test]
async fn auth_037_of_two_that_confirm_one_code_at_once_only_one_resets_the_password() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    ask(&api, &address).await;
    let code = code_sent(&api, &address).await;
    // The first confirmation, which locks the user's row and claims the code, as a confirmation
    // does, held open while the second runs.
    let mut first = api
        .database()
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{0}' FOR NO KEY UPDATE;
            DELETE FROM verification_codes WHERE user_id = '{0}'",
            ada.user_id
        ))
        .await;

    let second = tokio::spawn({
        let api = api.clone();
        let address = address.clone();
        async move { confirm(&api, &address, &code).await }
    });
    crate::support::queries_wait_on_locks(api.database(), 1).await;
    crate::support::execute_on(&mut first, "COMMIT").await;
    let answered = second.await.unwrap();

    assert_eq!(answered.status, 401);
    assert_eq!(answered.json()["code"], "auth.invalid_reset_code");
    let entries = log(&api, &ada.user_id).await;
    assert!(
        !entries
            .iter()
            .any(|entry| entry.starts_with("password.reset "))
    );
    assert_eq!(
        users::sign_in(&api, &address, users::PASSWORD).await.status,
        201
    );
}

#[tokio::test]
async fn auth_038_an_unknown_address_a_missing_code_an_expired_one_and_a_wrong_one_answer_alike() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let grace_address = mailbox::address("grace");
    let no_code_address = mailbox::address("hopper");
    let ada = users::register(&api, &address).await;
    let grace = users::register(&api, &grace_address).await;
    let no_code = users::register(&api, &no_code_address).await;
    ask(&api, &address).await;
    let code = code_sent(&api, &address).await;
    ask(&api, &grace_address).await;
    let grace_code = code_sent(&api, &grace_address).await;
    api.database()
        .owner_executes(&format!(
            "UPDATE verification_codes SET expires_at = now() - interval '1 second' \
             WHERE user_id = '{}'",
            grace.user_id
        ))
        .await;

    let unknown = confirm(&api, &mailbox::address("nobody"), &code).await;
    let missing = confirm(&api, &no_code_address, &code).await;
    let expired = confirm(&api, &grace_address, &grace_code).await;
    let wrong = confirm(&api, &address, &mailbox::other_grouped_code(&code)).await;

    let first = without_trace(unknown.json());
    assert_eq!(unknown.status, 401);
    assert_eq!(first["code"], "auth.invalid_reset_code");
    for answer in [&missing, &expired, &wrong] {
        assert_eq!(answer.status, 401);
        assert_eq!(without_trace(answer.json()), first);
    }
    for user in [&ada, &grace, &no_code] {
        assert_eq!(
            log(&api, &user.user_id).await.last().unwrap(),
            &format!("password_reset.failed - user {} {{}}", user.user_id)
        );
    }
    assert_eq!(confirm(&api, &address, &code).await.status, 204);
}

#[tokio::test]
async fn auth_038_a_malformed_address_code_and_password_answer_400_with_every_field() {
    let api = Api::new().await;

    let refused = api
        .post_raw(
            "/v1/password-resets/confirm",
            &json!({ "email": "ada", "code": "7KQM-2XWD-9RTB-H4NU", "password": "password1" }),
        )
        .send()
        .await;

    assert_eq!(refused.status, 400);
    let errors = refused.json()["errors"].clone();
    let fields: Vec<(&str, &str)> = errors
        .as_array()
        .unwrap()
        .iter()
        .map(|error| {
            (
                error["field"].as_str().unwrap(),
                error["code"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        fields,
        [
            ("/email", "invalid_email"),
            ("/code", "invalid_code"),
            ("/password", "breached")
        ]
    );
}

#[tokio::test]
async fn auth_038_with_email_off_a_confirmation_whose_body_converts_answers_503() {
    let api = Api::without_email().await;

    let converted = confirm(&api, "ada@example.com", "7KQM-2XWD-9RTB-H4NP").await;
    let malformed = api
        .post_raw(
            "/v1/password-resets/confirm",
            &json!({ "email": "ada@example.com", "code": "7KQM", "password": NEW_PASSWORD }),
        )
        .send()
        .await;

    assert_eq!(converted.status, 503);
    assert_eq!(converted.json()["code"], "request.feature_disabled");
    assert_eq!(malformed.status, 400);
}

#[tokio::test]
async fn auth_039_confirmations_count_per_email_from_each_client_address_before_any_work() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    ask(&api, &address).await;
    let code = code_sent(&api, &address).await;
    let wrong = mailbox::other_grouped_code(&code);
    let from = |peer: &'static str, code: String| {
        let api = api.clone();
        let address = address.clone();
        async move {
            api.post(
                "/v1/password-resets/confirm",
                &json!({ "email": address, "code": code, "password": NEW_PASSWORD }),
            )
            .from(peer)
            .send()
            .await
        }
    };

    let mut statuses = Vec::new();
    for _ in 0..5 {
        statuses.push(from("[2001:db8:1:2::1]:50000", wrong.clone()).await.status);
    }
    let refused = from("[2001:db8:1:2::2]:50000", code.clone()).await;
    let elsewhere = from("[2001:db8:1:3::1]:50000", code.clone()).await;

    assert_eq!(statuses, [401; 5]);
    assert_eq!(refused.status, 429, "the same /64");
    assert_eq!(refused.json()["code"], "request.rate_limited");
    assert!(
        refused
            .header("ratelimit-policy")
            .starts_with("\"reset_check\";q=5;w=900")
    );
    let failures = log(&api, &ada.user_id)
        .await
        .iter()
        .filter(|entry| entry.starts_with("password_reset.failed"))
        .count();
    assert_eq!(failures, 5, "the refused confirmation read nothing");
    assert_eq!(
        elsewhere.status, 204,
        "no count of the email from every address"
    );
}

#[tokio::test]
async fn auth_040_changing_sets_the_password_ends_the_other_sessions_and_keeps_the_caller_s() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let other = users::new_session(&api, &address).await;
    let before = api
        .get("/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await
        .json()["authenticated_at"]
        .clone();
    let mut signals = api.database().signals().await;

    let changed = change(&api, &ada, users::PASSWORD).await;

    assert_eq!(changed.status, 204);
    assert!(changed.body.is_empty());
    let refused = api.get("/v1/users/me").bearer(&other.access).send().await;
    assert_eq!(refused.status, 401);
    let session = api
        .get("/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(session.status, 200);
    assert!(session.json()["authenticated_at"].as_str() > before.as_str());
    let received: Vec<Value> = signals.received().await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["session_id"], other.session_id.as_str());
    let entries = log(&api, &ada.user_id).await;
    assert_eq!(
        entries[entries.len() - 2..],
        [
            format!(
                "session.ended {} session {} {{}}",
                ada.user_id, other.session_id
            ),
            format!(
                "password.changed {} identity {} {{}}",
                ada.user_id,
                identity(&api, &ada.user_id).await
            ),
        ]
    );
    assert_eq!(
        users::sign_in(&api, &address, users::PASSWORD).await.status,
        401
    );
    assert_eq!(
        users::sign_in(&api, &address, NEW_PASSWORD).await.status,
        201
    );
}

#[tokio::test]
async fn auth_046_a_change_and_a_reset_publish_the_end_of_each_session_they_end() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let other = users::new_session(&api, &address).await;

    let changed = change(&api, &ada, users::PASSWORD).await;
    let after_change = users::session_events(&api, "session.ended").await;
    let again = users::sign_in(&api, &address, NEW_PASSWORD).await;
    let again = SignedIn::from(&again);
    ask(&api, &address).await;
    let code = code_sent(&api, &address).await;
    let reset = confirm(&api, &address, &code).await;

    assert_eq!(changed.status, 204);
    assert_eq!(reset.status, 204);
    // The change ends the other session alone, and the reset every session left.
    assert_eq!(
        after_change,
        [format!("{} {}", ada.user_id, other.session_id)]
    );
    let mut ended = vec![
        format!("{} {}", ada.user_id, other.session_id),
        format!("{} {}", ada.user_id, ada.session_id),
        format!("{} {}", ada.user_id, again.session_id),
    ];
    ended.sort();
    assert_eq!(users::session_events(&api, "session.ended").await, ended);
}

#[tokio::test]
async fn auth_040_a_wrong_current_password_answers_403_and_changes_nothing_else() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let other = users::new_session(&api, &address).await;

    let refused = change(&api, &ada, "not-the-password-77").await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.wrong_password");
    assert_eq!(
        log(&api, &ada.user_id).await.last().unwrap(),
        &format!("password.check_failed {0} user {0} {{}}", ada.user_id)
    );
    let still = api.get("/v1/users/me").bearer(&other.access).send().await;
    assert_eq!(still.status, 200);
    assert_eq!(
        users::sign_in(&api, &address, users::PASSWORD).await.status,
        201
    );
}

#[tokio::test]
async fn auth_040_an_account_without_a_password_answers_403() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    api.database()
        .owner_executes("DELETE FROM identities")
        .await;

    let refused = change(&api, &ada, users::PASSWORD).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.wrong_password");
    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM identities")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_040_a_refused_new_password_and_an_empty_current_one_answer_400_with_both_fields() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let refused = api
        .post_raw(
            "/v1/users/me/password/change",
            &json!({ "current_password": "", "password": "AntWorker-rules-9" }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(refused.status, 400);
    let errors = refused.json()["errors"].clone();
    let fields: Vec<(&str, &str)> = errors
        .as_array()
        .unwrap()
        .iter()
        .map(|error| {
            (
                error["field"].as_str().unwrap(),
                error["code"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        fields,
        [
            ("/current_password", "required"),
            ("/password", "context_word")
        ]
    );
}

#[tokio::test]
async fn auth_022_a_change_counts_in_the_bucket_of_the_user_s_password_checks() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    for _ in 0..5 {
        let reauthenticated = api
            .post(
                "/v1/users/me/sessions/current/reauthenticate",
                &json!({ "password": "not-the-password-77" }),
            )
            .bearer(&ada.access)
            .send()
            .await;
        assert_eq!(reauthenticated.status, 403);
    }

    let refused = change(&api, &ada, users::PASSWORD).await;

    assert_eq!(refused.status, 429);
    assert!(
        refused
            .header("ratelimit-policy")
            .starts_with("\"password_check\";q=5;w=900")
    );
}

#[tokio::test]
async fn auth_041_a_reset_and_a_change_each_tell_the_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register(&api, &address).await;
    ask(&api, &address).await;
    let code = code_sent(&api, &address).await;
    confirm(&api, &address, &code).await;
    api.run_jobs().await;
    let signed_in = SignedIn::from(&users::sign_in(&api, &address, NEW_PASSWORD).await);

    let changed = api
        .post(
            "/v1/users/me/password/change",
            &json!({ "current_password": NEW_PASSWORD, "password": "quiet-harbor-73-amber" }),
        )
        .bearer(&signed_in.access)
        .send()
        .await;
    assert_eq!(changed.status, 204);
    api.run_jobs().await;

    let told: Vec<String> = mailbox::to(&address)
        .await
        .into_iter()
        .map(|email| email.subject)
        .filter(|subject| subject.contains("has changed"))
        .collect();
    assert_eq!(
        told,
        [
            "Your AntWorker password has changed",
            "Your AntWorker password has changed"
        ]
    );
}

#[tokio::test]
async fn auth_041_with_email_off_a_change_enqueues_no_email() {
    let api = Api::without_email().await;
    let ada = users::register(&api, "ada@example.com").await;

    assert_eq!(change(&api, &ada, users::PASSWORD).await.status, 204);

    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs")
            .await
            .is_empty()
    );
}
