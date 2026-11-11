//! Changing the account's email address with a code sent to the new one (the auth spec's
//! "Changing the email address", AUTH-048 to AUTH-053), and the turn a check of the password takes
//! (AUTH-022, AUTH-040).

use serde_json::json;

use crate::support::{
    Api, Options,
    api::Response,
    execute_on, mailbox, queries_wait_on_locks,
    users::{self, PASSWORD, SignedIn},
    workspaces,
};

/// Asks to change `user`'s address to `email`.
async fn ask(api: &Api, user: &SignedIn, email: &str) -> Response {
    api.post("/v1/users/me/email/change", &json!({ "email": email }))
        .bearer(&user.access)
        .send()
        .await
}

/// Confirms the change of `user`'s address to `email` with `code`.
async fn confirm(api: &Api, user: &SignedIn, email: &str, code: &str) -> Response {
    api.post(
        "/v1/users/me/email/change/confirm",
        &json!({ "email": email, "code": code }),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// The change code in the newest email with one that `address` received, once the API's jobs
/// have run.
async fn code_sent(api: &Api, address: &str) -> String {
    api.run_jobs().await;
    let emails = mailbox::to(address).await;
    let email = emails
        .iter()
        .find(|email| email.subject.contains("email change code"))
        .expect("an email with a change code");
    mailbox::grouped_code_in(&email.text).expect("a change code in the email")
}

/// `user`'s address and whether they proved it, as `address|true`.
async fn address_of(api: &Api, user: &SignedIn) -> String {
    api.database()
        .owner_reads(&format!(
            "SELECT email || '|' || email_verified FROM users WHERE id = '{}'",
            user.user_id
        ))
        .await
        .remove(0)
}

/// The purposes of `user`'s codes, sorted.
async fn codes_of(api: &Api, user: &SignedIn) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT purpose::text FROM verification_codes WHERE user_id = '{}' ORDER BY 1",
            user.user_id
        ))
        .await
}

/// Each job, as its kind, its user and its payload.
async fn jobs(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT kind || ' ' || coalesce(user_id::text, '-') || ' ' || payload::text \
             FROM jobs ORDER BY id",
        )
        .await
}

/// The entries of `user_id`'s log whose action is `action`: each one's actor, resource type,
/// resource ID and details, which SEC-18's check asks each action's test to assert.
async fn entries(api: &Api, user_id: &str, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT coalesce(actor_user_id::text, '-') || ' ' || resource_type \
             || ' ' || coalesce(resource_id::text, '-') || ' ' || details::text \
             FROM audit_entries WHERE user_id = '{user_id}' AND action = '{action}' ORDER BY id"
        ))
        .await
}

/// The statement of a lock that a test holds on `user`'s row, as a code's check, or a deletion of
/// the account, takes it first, which every confirmation of a change, and every check of the
/// password, waits for.
fn user_lock(user: &SignedIn) -> String {
    format!(
        "SELECT 1 FROM users WHERE id = '{}' FOR NO KEY UPDATE",
        user.user_id
    )
}

#[tokio::test]
async fn auth_048_asking_needs_a_recent_proof_then_enqueues_the_code_s_job_and_answers_202() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let new = mailbox::address("ada-new");
    users::authenticated_ago(&api, &ada, 6).await;

    let refused = ask(&api, &ada, &new).await;
    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.reauthentication_required");
    assert!(jobs(&api).await.is_empty());

    users::authenticated_ago(&api, &ada, 4).await;
    // Spaces around an address are outside its documented format, which `post_raw` sends anyway.
    let typed = format!("  {}  ", new.to_uppercase());
    let asked = api
        .post_raw("/v1/users/me/email/change", &json!({ "email": typed }))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(asked.status, 202);
    assert_eq!(asked.json(), json!({ "email": new }));
    assert_eq!(
        jobs(&api).await,
        [format!(
            "auth.send_verification_code {} {{\"email\": \"{new}\", \"purpose\": \"email_change\"}}",
            ada.user_id
        )]
    );
    // Until a confirmation, the account keeps its address.
    assert_eq!(address_of(&api, &ada).await, format!("{address}|true"));
}

#[tokio::test]
async fn auth_048_an_address_that_an_account_holds_answers_409_and_enqueues_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let grace = mailbox::address("grace");
    users::register(&api, &grace).await;

    for held in [grace.as_str(), address.as_str()] {
        let refused = ask(&api, &ada, held).await;
        assert_eq!(refused.status, 409, "{held}");
        assert_eq!(refused.json()["code"], "auth.email_in_use");
    }
    assert!(jobs(&api).await.is_empty());
}

#[tokio::test]
async fn auth_048_a_request_whose_email_isn_t_an_address_answers_400_at_email() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;

    let refused = api
        .post_raw("/v1/users/me/email/change", &json!({ "email": "ada" }))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(refused.status, 400);
    let errors = refused.json()["errors"].clone();
    assert_eq!(errors.as_array().unwrap().len(), 1);
    assert_eq!(errors[0]["field"], "/email");
    assert_eq!(errors[0]["code"], "invalid_email");
    assert!(jobs(&api).await.is_empty());
}

#[tokio::test]
async fn auth_048_with_email_off_asking_answers_503_once_its_body_converts() {
    let api = Api::without_email().await;
    let ada = users::register(&api, "ada@example.com").await;

    let asked = ask(&api, &ada, "ada@new.example.com").await;
    let malformed = api
        .post_raw(
            "/v1/users/me/email/change",
            &json!({ "email": "not an address" }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(asked.status, 503);
    assert_eq!(asked.json()["code"], "request.feature_disabled");
    assert_eq!(malformed.status, 400);
    assert!(jobs(&api).await.is_empty());
}

#[tokio::test]
async fn auth_025_an_account_that_must_verify_corrects_its_address_through_a_change() {
    let api = Api::new().await;
    let mistyped = mailbox::address("ada-mistyped");
    let ada = users::register_unverified(&api, &mistyped).await;
    let new = mailbox::address("ada");
    users::authenticated_ago(&api, &ada, 6).await;

    let reauthenticated = api
        .post(
            "/v1/users/me/sessions/current/reauthenticate",
            &json!({ "password": PASSWORD }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(reauthenticated.status, 200);
    assert_eq!(ask(&api, &ada, &new).await.status, 202);
    let code = code_sent(&api, &new).await;
    let confirmed = confirm(&api, &ada, &new, &code).await;

    assert_eq!(confirmed.status, 200);
    assert_eq!(confirmed.json()["email_verification_required"], false);
    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(me.status, 200);
    assert_eq!(me.json()["email"], new.as_str());
    assert_eq!(me.json()["email_verified"], true);
}

#[tokio::test]
async fn auth_049_asking_counts_in_a_bucket_of_the_address_before_it_reads_anything() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let grace = users::register(&api, &mailbox::address("grace")).await;
    let new = mailbox::address("new");
    // The requests for resets of the address fill a bucket of their own (AUTH-035).
    for _ in 0..3 {
        api.post("/v1/password-resets", &json!({ "email": new }))
            .send()
            .await;
    }
    api.database().owner_executes("DELETE FROM jobs").await;

    // The bucket is the address's, whichever account asks.
    for user in [&ada, &grace, &ada] {
        assert_eq!(ask(&api, user, &new).await.status, 202);
    }
    let refused = ask(&api, &grace, &new).await;
    assert_eq!(refused.status, 429);
    assert_eq!(refused.json()["code"], "request.rate_limited");
    assert_eq!(jobs(&api).await.len(), 3);
    // It counts before it reads anything: a request whose session's proof is old reads none.
    users::authenticated_ago(&api, &ada, 6).await;
    assert_eq!(ask(&api, &ada, &new).await.status, 429);

    users::authenticated_ago(&api, &ada, 0).await;
    assert_eq!(
        ask(&api, &ada, &mailbox::address("other")).await.status,
        202
    );
}

#[tokio::test]
async fn auth_050_the_job_stores_only_the_code_s_hmac_and_sends_the_code_to_the_address_asked_for()
{
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;

    let code = code_sent(&api, &new).await;

    let emails = mailbox::to(&new).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject, "Your AntWorker email change code");
    assert!(emails[0].html.contains(&code));
    // Nothing goes to the account's address until the change happens (AUTH-053).
    assert!(mailbox::to(&address).await.is_empty());
    let stored = api
        .database()
        .owner_reads(
            "SELECT id::text || ' ' || purpose || ' ' || encode(code_hash, 'hex') || ' ' \
             || (expires_at - created_at)::text FROM verification_codes",
        )
        .await;
    assert_eq!(stored.len(), 1);
    let fields: Vec<&str> = stored[0].split(' ').collect();
    let id = fields[0];
    assert_eq!(fields[1], "email_change");
    assert_eq!(fields[2].len(), 64);
    assert!(!fields[2].contains(&code.replace('-', "").to_lowercase()));
    assert_eq!(fields[3], "00:10:00");
    assert_eq!(
        entries(&api, &ada.user_id, "email_change_code.issued").await,
        [format!("- verification_code {id} {{}}")]
    );
}

#[tokio::test]
async fn auth_050_a_newer_code_replaces_the_one_before_it() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let first = mailbox::address("first");
    let second = mailbox::address("second");
    ask(&api, &ada, &first).await;
    let first_code = code_sent(&api, &first).await;
    ask(&api, &ada, &second).await;
    let second_code = code_sent(&api, &second).await;

    let replaced = confirm(&api, &ada, &first, &first_code).await;
    let confirmed = confirm(&api, &ada, &second, &second_code).await;

    assert_eq!(replaced.status, 422);
    assert_eq!(replaced.json()["code"], "auth.wrong_code");
    assert_eq!(confirmed.status, 200);
    assert_eq!(address_of(&api, &ada).await, format!("{second}|true"));
}

#[tokio::test]
async fn auth_050_a_run_that_another_run_beat_to_the_next_code_issues_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    // Another run's code, which its unit of work holds uncommitted while this run issues its own:
    // the unique index makes this run's insert wait, then do nothing (ERR-05).
    let mut other = api
        .database()
        .holding(&format!(
            "INSERT INTO verification_codes (user_id, purpose, code_hash, expires_at) \
             VALUES ('{}', 'email_change', '\\x0123abcd', now() + interval '10 minutes')",
            ada.user_id
        ))
        .await;

    let run = tokio::spawn({
        let api = api.clone();
        async move { api.run_jobs().await }
    });
    queries_wait_on_locks(api.database(), 1).await;
    execute_on(&mut other, "COMMIT").await;

    assert_eq!(run.await.unwrap(), 1);
    assert!(mailbox::to(&new).await.is_empty());
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT encode(code_hash, 'hex') || ' ' || (SELECT count(*) FROM audit_entries \
                 WHERE action = 'email_change_code.issued') FROM verification_codes"
            )
            .await,
        ["0123abcd 0"]
    );
}

#[tokio::test]
async fn auth_050_a_run_for_an_account_that_is_gone_issues_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    api.database()
        .owner_executes(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    assert_eq!(api.run_jobs().await, 1);

    assert!(mailbox::to(&new).await.is_empty());
}

#[tokio::test]
async fn auth_050_a_run_on_a_replica_with_email_off_issues_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let off = api.instance(Options::email_off()).await;

    assert_eq!(off.run_jobs().await, 1);

    assert!(codes_of(&api, &ada).await.is_empty());
    assert!(mailbox::to(&new).await.is_empty());
}

#[tokio::test]
async fn auth_051_the_code_changes_the_address_proves_it_and_deletes_every_code() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let db = api.database();
    let acme = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    // A reset code, sent to the address before the change.
    api.post("/v1/password-resets", &json!({ "email": address }))
        .send()
        .await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;
    assert_eq!(
        codes_of(&api, &ada).await,
        ["email_change", "password_reset"]
    );
    db.owner_executes("DELETE FROM events").await;

    let confirmed = confirm(&api, &ada, &new, &code.to_lowercase()).await;

    assert_eq!(confirmed.status, 200);
    assert_eq!(confirmed.json()["id"], ada.session_id);
    assert_eq!(confirmed.json()["email_verification_required"], false);
    assert!(confirmed.json().get("access_token").is_none());
    assert_eq!(address_of(&api, &ada).await, format!("{new}|true"));
    assert!(codes_of(&api, &ada).await.is_empty());
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api, ada_id, "email.changed").await,
        [format!("{ada_id} user {ada_id} {{}}")]
    );
    let events = db
        .owner_reads(
            "SELECT name || ' ' || coalesce(user_id::text, '-') || ' ' \
             || coalesce(workspace_id::text, '-') || ' ' || payload::text \
             FROM events ORDER BY name",
        )
        .await;
    assert_eq!(
        events,
        [
            format!(
                "member.updated - {acme} {{\"user_id\": \"{ada_id}\", \"workspace_id\": \"{acme}\"}}"
            ),
            format!("user.updated {ada_id} - {{\"user_id\": \"{ada_id}\"}}"),
        ]
    );
    assert_eq!(users::sign_in(&api, &new, PASSWORD).await.status, 201);
    assert_eq!(users::sign_in(&api, &address, PASSWORD).await.status, 401);
}

#[tokio::test]
async fn auth_051_of_two_that_confirm_one_code_at_once_only_one_changes_the_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let phone = users::new_session(&api, &address).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;
    let db = api.database();
    let mut held = api.database().holding(&user_lock(&ada)).await;
    let body = json!({ "email": new, "code": code });

    let first = api.spawn(
        http::Method::POST,
        "/v1/users/me/email/change/confirm",
        &ada.access,
        Some(body.clone()),
    );
    let second = api.spawn(
        http::Method::POST,
        "/v1/users/me/email/change/confirm",
        &phone.access,
        Some(body),
    );
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut held, "COMMIT").await;
    let mut answers = [first.await.unwrap(), second.await.unwrap()];
    answers.sort_by_key(|(status, _)| status.as_u16());

    assert_eq!(answers[0].0, 200);
    assert_eq!(answers[1].0, 422);
    assert_eq!(answers[1].1["code"], "auth.code_expired");
    assert_eq!(address_of(&api, &ada).await, format!("{new}|true"));
}

#[tokio::test]
async fn auth_051_a_session_that_another_request_ended_meanwhile_answers_401_and_changes_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;
    let db = api.database();
    let mut held = api.database().holding(&user_lock(&ada)).await;

    let confirmed = api.spawn(
        http::Method::POST,
        "/v1/users/me/email/change/confirm",
        &ada.access,
        Some(json!({ "email": new, "code": code })),
    );
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut held,
        &format!("DELETE FROM sessions WHERE id = '{}'", ada.session_id),
    )
    .await;
    execute_on(&mut held, "COMMIT").await;
    let (status, body) = confirmed.await.unwrap();

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert_eq!(address_of(&api, &ada).await, format!("{address}|true"));
    assert_eq!(codes_of(&api, &ada).await, ["email_change"]);
}

#[tokio::test]
async fn auth_051_a_confirmation_that_waited_for_the_account_s_deletion_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;
    let db = api.database();
    let mut deletion = db
        .holding(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    let confirmed = api.spawn(
        http::Method::POST,
        "/v1/users/me/email/change/confirm",
        &ada.access,
        Some(json!({ "email": new, "code": code })),
    );
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, body) = confirmed.await.unwrap();

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
}

#[tokio::test]
async fn auth_038_a_reset_code_sent_to_the_address_before_a_change_resets_nothing_after_it() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let change_code = code_sent(&api, &new).await;
    let db = api.database();
    // An expired reset code, whose row the test holds: the job of the next reset deletes it once
    // it has read the account's address, and so does the change, as it deletes every code.
    let expired = db
        .owner_reads(&format!(
            "INSERT INTO verification_codes (user_id, purpose, code_hash, expires_at) \
             VALUES ('{}', 'password_reset', '\\x00', now() - interval '1 minute') \
             RETURNING id::text",
            ada.user_id
        ))
        .await
        .remove(0);
    let mut held = db
        .holding(&format!(
            "SELECT 1 FROM verification_codes WHERE id = '{expired}' FOR UPDATE"
        ))
        .await;
    api.post("/v1/password-resets", &json!({ "email": address }))
        .send()
        .await;

    let jobs = tokio::spawn({
        let api = api.clone();
        async move { api.run_jobs().await }
    });
    queries_wait_on_locks(db, 1).await;
    let changed = api.spawn(
        http::Method::POST,
        "/v1/users/me/email/change/confirm",
        &ada.access,
        Some(json!({ "email": new, "code": change_code })),
    );
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut held, "COMMIT").await;
    jobs.await.unwrap();
    assert_eq!(changed.await.unwrap().0, 200);

    // The job read the address before the change, and sent its code there, after the change had
    // passed the codes it deletes.
    assert_eq!(codes_of(&api, &ada).await, ["password_reset"]);
    let emails = mailbox::to(&address).await;
    let reset = emails
        .iter()
        .find(|email| email.subject.contains("password reset code"))
        .expect("the reset code's email");
    let reset_code = mailbox::grouped_code_in(&reset.text).unwrap();
    for email in [new.as_str(), address.as_str()] {
        let refused = api
            .post(
                "/v1/password-resets/confirm",
                &json!({ "email": email, "code": reset_code, "password": "lantern-orchard-58-brisk" }),
            )
            .send()
            .await;
        assert_eq!(refused.status, 401, "{email}");
        assert_eq!(refused.json()["code"], "auth.invalid_reset_code");
    }
    assert_eq!(users::sign_in(&api, &new, PASSWORD).await.status, 201);
}

#[tokio::test]
async fn auth_052_a_wrong_code_and_the_right_code_with_another_address_answer_422() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;
    let id = api
        .database()
        .owner_reads("SELECT id::text FROM verification_codes")
        .await
        .remove(0);

    let wrong = confirm(&api, &ada, &new, &mailbox::other_grouped_code(&code)).await;
    let elsewhere = confirm(&api, &ada, &mailbox::address("elsewhere"), &code).await;

    for refused in [&wrong, &elsewhere] {
        assert_eq!(refused.status, 422);
        assert_eq!(refused.json()["code"], "auth.wrong_code");
    }
    let ada_id = &ada.user_id;
    assert_eq!(
        entries(&api, ada_id, "verification_code.check_failed").await,
        [
            format!("{ada_id} verification_code {id} {{}}"),
            format!("{ada_id} verification_code {id} {{}}"),
        ]
    );
    // The code is as it was.
    assert_eq!(confirm(&api, &ada, &new, &code).await.status, 200);
}

#[tokio::test]
async fn auth_052_no_code_and_an_expired_one_answer_code_expired() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let new = mailbox::address("ada-new");

    let none = confirm(&api, &ada, &new, "7KQM-2XWD-9RTB-H4NP").await;
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;
    api.database()
        .owner_executes("UPDATE verification_codes SET expires_at = now()")
        .await;
    let expired = confirm(&api, &ada, &new, &code).await;

    for refused in [&none, &expired] {
        assert_eq!(refused.status, 422);
        assert_eq!(refused.json()["code"], "auth.code_expired");
    }
    assert!(
        entries(&api, &ada.user_id, "verification_code.check_failed")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_052_an_address_that_another_account_took_meanwhile_answers_409_and_changes_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;
    users::register(&api, &new).await;

    let refused = confirm(&api, &ada, &new, &code).await;

    assert_eq!(refused.status, 409);
    assert_eq!(refused.json()["code"], "auth.email_in_use");
    assert_eq!(address_of(&api, &ada).await, format!("{address}|true"));
    assert_eq!(codes_of(&api, &ada).await, ["email_change"]);
    assert!(
        entries(&api, &ada.user_id, "email.changed")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_052_a_malformed_email_and_code_answer_400_with_both_fields() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;

    let refused = api
        .post_raw(
            "/v1/users/me/email/change/confirm",
            &json!({ "email": "ada", "code": "7KQM-2XWD-9RTB-H4NU" }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(refused.status, 400);
    let fields: Vec<String> = refused.json()["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| format!("{} {}", error["field"], error["code"]))
        .collect();
    assert_eq!(
        fields,
        ["\"/email\" \"invalid_email\"", "\"/code\" \"invalid_code\""]
    );
}

#[tokio::test]
async fn auth_052_with_email_off_a_confirmation_whose_body_converts_answers_503() {
    let api = Api::without_email().await;
    let ada = users::register(&api, "ada@example.com").await;

    let confirmed = confirm(&api, &ada, "ada@new.example.com", "7KQM-2XWD-9RTB-H4NP").await;

    assert_eq!(confirmed.status, 503);
    assert_eq!(confirmed.json()["code"], "request.feature_disabled");
}

#[tokio::test]
async fn auth_033_a_change_s_checks_count_in_the_bucket_of_the_user_s_code_checks() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;

    for _ in 0..9 {
        assert_eq!(
            confirm(&api, &ada, &new, &mailbox::other_grouped_code(&code))
                .await
                .status,
            422
        );
    }
    // A verification code's check counts in the same bucket.
    let verification = api
        .post(
            "/v1/users/me/email/verification/confirm",
            &json!({ "code": "123456" }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(verification.status, 409);
    let refused = confirm(&api, &ada, &new, &code).await;

    assert_eq!(refused.status, 429);
    assert_eq!(refused.json()["code"], "request.rate_limited");
    assert_ne!(address_of(&api, &ada).await, format!("{new}|true"));
}

#[tokio::test]
async fn auth_053_a_change_of_a_proved_address_tells_it_and_names_the_new_one() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;

    assert_eq!(confirm(&api, &ada, &new, &code).await.status, 200);
    api.run_jobs().await;

    let emails = mailbox::to(&address).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(
        emails[0].subject,
        "Your AntWorker email address has changed"
    );
    assert!(
        emails[0]
            .text
            .contains(&format!("has changed from {address} to {new}")),
        "{}",
        emails[0].text
    );
    // The new address got its code alone.
    assert_eq!(mailbox::to(&new).await.len(), 1);
}

#[tokio::test]
async fn auth_053_a_change_of_an_address_the_account_hadn_t_proved_tells_no_one() {
    let api = Api::new().await;
    let mistyped = mailbox::address("ada-mistyped");
    let ada = users::register_unverified(&api, &mistyped).await;
    let new = mailbox::address("ada");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;

    assert_eq!(confirm(&api, &ada, &new, &code).await.status, 200);
    api.run_jobs().await;

    // The registration's code went there, and nothing else.
    let emails = mailbox::to(&mistyped).await;
    assert!(
        emails
            .iter()
            .all(|email| email.subject == "Your AntWorker verification code"),
        "{emails:?}"
    );
}

#[tokio::test]
async fn auth_040_of_two_changes_sent_at_once_the_second_checks_against_the_first_s_password() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let mut held = api.database().holding(&user_lock(&ada)).await;
    let path = "/v1/users/me/password/change";
    let to = |password: &str| json!({ "current_password": PASSWORD, "password": password });

    let first = api.spawn(
        http::Method::POST,
        path,
        &ada.access,
        Some(to("lantern-orchard-58-brisk")),
    );
    let second = api.spawn(
        http::Method::POST,
        path,
        &ada.access,
        Some(to("meadow-copper-73-swift")),
    );
    // Each waits for the user's turn before it reads the password.
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut held, "COMMIT").await;
    let (first, second) = (first.await.unwrap(), second.await.unwrap());

    let mut statuses = [first.0.as_u16(), second.0.as_u16()];
    statuses.sort_unstable();
    assert_eq!(statuses, [204, 403]);
    let (changed, refused) = if first.0 == 204 {
        ("lantern-orchard-58-brisk", second)
    } else {
        ("meadow-copper-73-swift", first)
    };
    assert_eq!(refused.1["code"], "auth.wrong_password");
    assert_eq!(
        users::sign_in(&api, "ada@example.com", changed)
            .await
            .status,
        201
    );
}

#[tokio::test]
async fn auth_048_a_session_that_another_request_ended_before_its_proof_is_read_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let new = mailbox::address("ada-new");
    let db = api.database();
    // Every bucket of Ada's requests but the change's, which a first request then adds.
    api.get("/v1/users/me").bearer(&ada.access).send().await;
    let change_bucket = users::added_bucket(&api, ask(&api, &ada, &new)).await;
    // The request waits at its bucket, once it is authenticated, while another request ends its
    // session.
    let mut ender = db
        .holding(&format!(
            "SELECT 1 FROM rate_limit_buckets WHERE id = '{change_bucket}' FOR UPDATE"
        ))
        .await;

    let asked = api.spawn(
        http::Method::POST,
        "/v1/users/me/email/change",
        &ada.access,
        Some(json!({ "email": new })),
    );
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut ender,
        &format!("DELETE FROM sessions WHERE id = '{}'", ada.session_id),
    )
    .await;
    execute_on(&mut ender, "COMMIT").await;
    let (status, body) = asked.await.unwrap();

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    // The first request's job alone.
    assert_eq!(jobs(&api).await.len(), 1);
}

#[tokio::test]
async fn auth_049_requests_for_changes_and_the_buckets_of_registrations_and_codes_count_apart() {
    let api = Api::new().await;
    let new = mailbox::address("new");
    // The address's registrations and requests for codes fill their buckets (AUTH-029): one
    // registration and three requests for codes by its account, then two registrations more.
    let holder = users::register_unverified(&api, &new).await;
    for _ in 0..3 {
        let asked = api
            .request(http::Method::POST, "/v1/users/me/email/verification")
            .bearer(&holder.access)
            .send()
            .await;
        assert_eq!(asked.status, 202);
    }
    for _ in 0..2 {
        let again = api
            .post("/v1/users", &users::registration(&new))
            .send()
            .await;
        assert_eq!(again.status, 409);
    }
    let db = api.database();
    db.owner_executes(&format!(
        "DELETE FROM users WHERE id = '{}'; DELETE FROM jobs",
        holder.user_id
    ))
    .await;
    let ada = users::register(&api, &mailbox::address("ada")).await;

    for _ in 0..3 {
        assert_eq!(ask(&api, &ada, &new).await.status, 202);
    }
    assert_eq!(ask(&api, &ada, &new).await.status, 429);
}

#[tokio::test]
async fn auth_050_an_attempt_after_a_failed_send_issues_a_new_code_in_place_of_its_own() {
    let unreachable = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let failing = Api::with(Options::default().set("SMTP_PORT", unreachable.port())).await;
    let ada = users::register(&failing, &mailbox::address("ada")).await;
    let new = mailbox::address("ada-new");
    ask(&failing, &ada, &new).await;
    assert_eq!(failing.run_jobs().await, 1);
    let db = failing.database();
    let first = db
        .owner_reads("SELECT id::text FROM verification_codes")
        .await;
    assert_eq!(first.len(), 1);
    assert_eq!(
        db.owner_reads("SELECT state::text || ' ' || attempts FROM jobs")
            .await,
        ["waiting 1"]
    );
    // The next attempt, on a replica that reaches the mail server.
    db.owner_executes("UPDATE jobs SET run_at = now()").await;
    let reaching = failing.instance(Options::default()).await;

    assert_eq!(reaching.run_jobs().await, 1);

    let second = db
        .owner_reads("SELECT id::text FROM verification_codes")
        .await;
    assert_eq!(second.len(), 1);
    assert_ne!(second, first);
    assert_eq!(mailbox::to(&new).await.len(), 1);
    assert_eq!(
        entries(&failing, &ada.user_id, "email_change_code.issued")
            .await
            .len(),
        2
    );
    // The code that the email carries is the second attempt's.
    let code = code_sent(&failing, &new).await;
    assert_eq!(confirm(&failing, &ada, &new, &code).await.status, 200);
}

#[tokio::test]
async fn auth_052_a_code_that_a_newer_one_replaces_while_the_check_runs_answers_code_expired() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let new = mailbox::address("ada-new");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;
    // A newer code's job deletes the code, and holds it while this check runs, then commits, as
    // the job that replaces it does.
    let mut holder = api
        .database()
        .holding(&format!(
            "DELETE FROM verification_codes WHERE user_id = '{}' AND purpose = 'email_change'",
            ada.user_id
        ))
        .await;

    let confirmed = api.spawn(
        http::Method::POST,
        "/v1/users/me/email/change/confirm",
        &ada.access,
        Some(json!({ "email": new, "code": code })),
    );
    queries_wait_on_locks(api.database(), 1).await;
    execute_on(&mut holder, "COMMIT").await;
    let (status, body) = confirmed.await.unwrap();

    assert_eq!(status, 422);
    assert_eq!(body["code"], "auth.code_expired");
    assert_eq!(address_of(&api, &ada).await, format!("{address}|true"));
}

#[tokio::test]
async fn auth_026_re_authentication_s_answer_says_whether_the_account_must_prove_its_address() {
    let api = Api::new().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;

    let reauthenticated = api
        .post(
            "/v1/users/me/sessions/current/reauthenticate",
            &json!({ "password": PASSWORD }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(reauthenticated.status, 200);
    assert_eq!(reauthenticated.json()["email_verification_required"], true);
}

#[tokio::test]
async fn auth_026_a_change_s_confirmation_says_the_account_needn_t_prove_its_address() {
    let api = Api::new().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada-mistyped")).await;
    let new = mailbox::address("ada");
    ask(&api, &ada, &new).await;
    let code = code_sent(&api, &new).await;

    let confirmed = confirm(&api, &ada, &new, &code).await;

    assert_eq!(confirmed.status, 200);
    assert_eq!(confirmed.json()["email_verification_required"], false);
    let session = api
        .get("/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(session.json()["email_verification_required"], false);
}

#[tokio::test]
async fn auth_022_a_check_that_waits_for_a_change_or_a_reset_reads_the_password_it_set() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let db = api.database();
    let changed_to = "lantern-orchard-58-brisk";
    let reset_to = "meadow-copper-73-swift";
    let reauthenticate = |password: &str| {
        api.spawn(
            http::Method::POST,
            "/v1/users/me/sessions/current/reauthenticate",
            &ada.access,
            Some(json!({ "password": password })),
        )
    };

    // A re-authentication with the password a change replaces, which waits for the change.
    let mut held = db.holding(&user_lock(&ada)).await;
    let change = api.spawn(
        http::Method::POST,
        "/v1/users/me/password/change",
        &ada.access,
        Some(json!({ "current_password": PASSWORD, "password": changed_to })),
    );
    queries_wait_on_locks(db, 1).await;
    let after_change = reauthenticate(PASSWORD);
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut held, "COMMIT").await;
    assert_eq!(change.await.unwrap().0, 204);
    let (status, body) = after_change.await.unwrap();
    assert_eq!(status, 403);
    assert_eq!(body["code"], "auth.wrong_password");

    // A re-authentication with the password a reset replaces, which waits for the reset.
    api.post("/v1/password-resets", &json!({ "email": address }))
        .send()
        .await;
    api.run_jobs().await;
    let emails = mailbox::to(&address).await;
    let reset_email = emails
        .iter()
        .find(|email| email.subject.contains("password reset code"))
        .expect("the reset code's email");
    let reset_code = mailbox::grouped_code_in(&reset_email.text).unwrap();
    let mut held = db.holding(&user_lock(&ada)).await;
    let reset = tokio::spawn({
        let api = api.clone();
        let body = json!({ "email": address, "code": reset_code, "password": reset_to });
        async move {
            api.post("/v1/password-resets/confirm", &body)
                .send()
                .await
                .status
        }
    });
    queries_wait_on_locks(db, 1).await;
    let after_reset = reauthenticate(changed_to);
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut held, "COMMIT").await;
    assert_eq!(reset.await.unwrap(), 204);
    let (status, body) = after_reset.await.unwrap();
    assert_eq!(status, 403);
    assert_eq!(body["code"], "auth.wrong_password");
}

#[tokio::test]
async fn auth_040_of_two_changes_sent_at_once_that_keep_the_password_both_answer_204() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let mut held = db.holding(&user_lock(&ada)).await;
    let keep = json!({ "current_password": PASSWORD, "password": PASSWORD });

    let first = api.spawn(
        http::Method::POST,
        "/v1/users/me/password/change",
        &ada.access,
        Some(keep.clone()),
    );
    let second = api.spawn(
        http::Method::POST,
        "/v1/users/me/password/change",
        &ada.access,
        Some(keep),
    );
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut held, "COMMIT").await;

    // The first kept the password it had, which the second's check then reads.
    assert_eq!(first.await.unwrap().0, 204);
    assert_eq!(second.await.unwrap().0, 204);
    assert_eq!(
        users::sign_in(&api, "ada@example.com", PASSWORD)
            .await
            .status,
        201
    );
}
