//! A deployment's requirement of a second factor of every account, and the operator's removal of
//! a lost one (the auth spec's "Second factors", AUTH-070 to AUTH-072).

use std::process::Output;

use serde_json::json;

use crate::support::{
    Api, Options, api,
    api::Response,
    mailbox,
    second_factors::{self, add_app, code, confirm, start_setup},
    users::{self, PASSWORD, SignedIn},
    workspaces,
};

/// The API of a deployment that requires a second factor of every account (D133).
async fn requiring() -> Api {
    Api::with(Options::default().set("SECOND_FACTOR_REQUIRED", true)).await
}

/// `user`'s current session, as AUTH-016 reads it.
async fn current(api: &Api, user: &SignedIn) -> Response {
    api.get("/v1/users/me/sessions/current")
        .bearer(&user.access)
        .send()
        .await
}

/// The first column of each row of `sql`, as the `owner` reads it.
async fn reads(api: &Api, sql: &str) -> Vec<String> {
    api.database().owner_reads(sql).await
}

/// Runs the binary's `remove-second-factor` for `email` on `api`'s database, with email on, sent
/// through the tests' Mailpit, as `api`'s own settings send it.
async fn remove(api: &Api, email: &str) -> Output {
    api.database()
        .run_api(
            &["remove-second-factor", email],
            &[
                ("ENVIRONMENT", "test"),
                ("EMAIL", "smtp"),
                ("SMTP_HOST", "127.0.0.1"),
                ("SMTP_PORT", "1025"),
                ("SMTP_SECURITY", "none"),
                ("EMAIL_FROM", "AntWorker <noreply@example.com>"),
            ],
        )
        .await
}

/// What `output` wrote, its logs among it, to stdout and stderr together.
fn printed(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Asserts that `output` exited 1 with the one failure that OPS-03 gives a subcommand, whose
/// message is `message`, and that it never wrote `address`, since the logs hold no personal data
/// (OBS-04).
fn refused(output: &Output, message: &str, address: &str) {
    let written = printed(output);
    assert_eq!(output.status.code(), Some(1), "{written}");
    assert!(written.contains("startup failed"), "{written}");
    assert!(written.contains("remove-second-factor"), "{written}");
    assert!(written.contains(message), "{written}");
    assert!(!written.contains(address), "{written}");
}

#[tokio::test]
async fn auth_070_an_account_without_an_app_is_refused_but_where_setting_one_up_needs() {
    let api = requiring().await;
    let ada = users::register(&api, "ada@example.com").await;
    let acme = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;

    for path in [
        "/v1/users/me".to_owned(),
        "/v1/users/me/sessions".to_owned(),
        format!("/v1/workspaces/{acme}"),
    ] {
        let refused = api.get(&path).bearer(&ada.access).send().await;
        assert_eq!(refused.status, 403, "{path}");
        assert_eq!(
            refused.json()["code"],
            "request.second_factor_setup_required",
            "{path}"
        );
    }
    let created = api
        .post("/v1/workspaces", &json!({ "name": "Side" }))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(created.status, 403);
    assert_eq!(
        created.json()["code"],
        "request.second_factor_setup_required"
    );

    // The routes that verifying an address needs pass, the session's among them, and those that
    // setting an app up needs.
    let session = current(&api, &ada).await;
    assert_eq!(session.status, 200);
    assert_eq!(session.json()["second_factor_setup_required"], true);
    let reauthenticated = api
        .post(
            "/v1/users/me/sessions/current/reauthenticate",
            &json!({ "password": PASSWORD }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(reauthenticated.status, 200);
    let factors = api
        .get("/v1/users/me/second-factors")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(factors.status, 200);
    let setup = start_setup(&api, &ada).await;
    assert_eq!(setup.status, 200);
    let secret = setup.json()["secret"].as_str().unwrap().to_owned();
    assert_eq!(
        confirm(&api, &ada, &code(&api, &secret).await).await.status,
        200
    );

    // The app counts from the next request on.
    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(me.status, 200);
    let workspace = api
        .get(&format!("/v1/workspaces/{acme}"))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(workspace.status, 200);
    assert_eq!(
        current(&api, &ada).await.json()["second_factor_setup_required"],
        false
    );
    let signed_out = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(signed_out.status, 204);
}

#[tokio::test]
async fn auth_070_an_account_that_must_prove_its_address_is_refused_for_that_first() {
    let api = requiring().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;

    let factors = api
        .get("/v1/users/me/second-factors")
        .bearer(&ada.access)
        .send()
        .await;
    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;
    let session = current(&api, &ada).await;

    assert_eq!(factors.status, 403);
    assert_eq!(factors.json()["code"], "request.email_unverified");
    assert_eq!(me.status, 403);
    assert_eq!(me.json()["code"], "request.email_unverified");
    assert_eq!(session.status, 200);
    assert_eq!(session.json()["email_verification_required"], true);
    assert_eq!(session.json()["second_factor_setup_required"], true);
}

#[tokio::test]
async fn auth_070_every_answer_that_holds_a_session_says_whether_its_account_must_set_up_an_app() {
    let api = requiring().await;
    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    assert_eq!(registered.status, 201);
    assert_eq!(registered.json()["second_factor_setup_required"], true);
    let ada = SignedIn::from(&registered);
    users::verified(&api, &ada.user_id).await;
    let signed_in = users::sign_in(&api, "ada@example.com", PASSWORD).await;
    assert_eq!(signed_in.status, 201);
    assert_eq!(signed_in.json()["second_factor_setup_required"], true);
    let refreshed = api
        .post(
            "/v1/sessions/refresh",
            &json!({ "refresh_token": ada.refresh }),
        )
        .send()
        .await;
    assert_eq!(refreshed.status, 200);
    assert_eq!(refreshed.json()["second_factor_setup_required"], true);
    let ada = SignedIn {
        access: refreshed.json()["access_token"]
            .as_str()
            .unwrap()
            .to_owned(),
        refresh: refreshed.json()["refresh_token"]
            .as_str()
            .unwrap()
            .to_owned(),
        ..ada
    };

    let app = add_app(&api, &ada).await;
    let reauthenticated = api
        .post(
            "/v1/users/me/sessions/current/reauthenticate",
            &json!({ "password": PASSWORD, "code": code(&api, &app.secret).await }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(reauthenticated.status, 200);
    assert_eq!(
        reauthenticated.json()["second_factor_setup_required"],
        false
    );
    second_factors::forget_steps(&api, &ada.user_id).await;
    let challenge = second_factors::challenge(&api, "ada@example.com").await;
    let completed = second_factors::complete(
        &api,
        &challenge,
        json!({ "code": code(&api, &app.secret).await }),
    )
    .await;
    assert_eq!(completed.status, 201);
    assert_eq!(completed.json()["second_factor_setup_required"], false);

    // Off by default, the answers say false.
    let off = Api::new().await;
    let registered = off
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    assert_eq!(registered.json()["second_factor_setup_required"], false);
}

#[tokio::test]
async fn auth_070_the_confirmations_of_a_code_and_of_a_change_of_the_address_say_it_too() {
    let api = requiring().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    let asked = api
        .request(http::Method::POST, "/v1/users/me/email/verification")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(asked.status, 202);
    api.run_jobs().await;
    let code = mailbox::to(&address)
        .await
        .first()
        .and_then(|email| {
            email
                .text
                .split_whitespace()
                .find(|word| word.len() == 6 && word.bytes().all(|byte| byte.is_ascii_digit()))
                .map(str::to_owned)
        })
        .expect("a verification code");

    let verified = api
        .post(
            "/v1/users/me/email/verification/confirm",
            &json!({ "code": code }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(verified.status, 200, "{}", verified.json());
    assert_eq!(verified.json()["email_verification_required"], false);
    assert_eq!(verified.json()["second_factor_setup_required"], true);

    let new = mailbox::address("ada-new");
    let asked = api
        .post("/v1/users/me/email/change", &json!({ "email": new }))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(asked.status, 202, "{}", asked.json());
    api.run_jobs().await;
    let code = mailbox::to(&new)
        .await
        .iter()
        .find(|email| email.subject.contains("email change code"))
        .and_then(|email| mailbox::grouped_code_in(&email.text))
        .expect("a change code");

    let changed = api
        .post(
            "/v1/users/me/email/change/confirm",
            &json!({ "email": new, "code": code }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(changed.status, 200, "{}", changed.json());
    assert_eq!(changed.json()["second_factor_setup_required"], true);
}

#[tokio::test]
async fn auth_070_the_setting_is_true_or_false() {
    let refusal = api::refusal(&Options::default().set("SECOND_FACTOR_REQUIRED", "yes"));

    assert_eq!(refusal, "SECOND_FACTOR_REQUIRED must be true or false");
}

#[tokio::test]
async fn auth_071_with_the_setting_on_removing_the_app_answers_422_and_changes_nothing() {
    let api = requiring().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    let before = reads(&api, "SELECT id::text FROM totp_factors").await;
    let remove_app = || async {
        api.request(http::Method::DELETE, "/v1/users/me/second-factors/totp")
            .bearer(&ada.access)
            .send()
            .await
    };

    let before_buckets = users::buckets(&api).await;
    let refused = remove_app().await;
    // It counted in the bucket that asking for new recovery codes shares (AUTH-064), which that
    // request then adds nothing to.
    let counted = users::buckets(&api).await;
    assert_eq!(counted.len(), before_buckets.len() + 1, "{counted:?}");
    let codes = api
        .request(
            http::Method::POST,
            "/v1/users/me/second-factors/recovery-codes",
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(codes.status, 200, "{}", codes.json());
    assert_eq!(users::buckets(&api).await, counted);
    // Before the proof is read, so a session that proved nothing lately is told the same.
    users::authenticated_ago(&api, &ada, 10).await;
    let stale = remove_app().await;
    users::authenticated_ago(&api, &ada, 0).await;

    assert_eq!(stale.status, 422);
    assert_eq!(stale.json()["code"], "auth.second_factor_mandatory");
    assert_eq!(refused.status, 422);
    assert_eq!(refused.json()["code"], "auth.second_factor_mandatory");
    assert_eq!(
        reads(&api, "SELECT id::text FROM totp_factors").await,
        before
    );
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM recovery_codes").await,
        ["10"]
    );
    assert!(
        reads(
            &api,
            "SELECT id::text FROM audit_entries WHERE action = 'second_factor.removed'"
        )
        .await
        .is_empty()
    );
    // The account replaces it by setting up another.
    let replaced = add_app(&api, &ada).await;
    assert!(!replaced.secret.is_empty());
    let after = reads(&api, "SELECT id::text FROM totp_factors").await;
    assert_eq!(after.len(), 1);
    assert_ne!(after, before);
}

/// The log, actor, resource type and resource ID of each entry of `action`, the actor `-` when
/// there is none.
async fn entries(api: &Api, action: &str) -> Vec<String> {
    reads(
        api,
        &format!(
            "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_type \
             || ' ' || coalesce(resource_id::text, '-') FROM audit_entries \
             WHERE action = '{action}' ORDER BY created_at"
        ),
    )
    .await
}

#[tokio::test]
async fn auth_072_the_subcommand_removes_the_app_and_the_codes_and_ends_every_session() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    add_app(&api, &ada).await;
    // A sign-in that waits for its second factor.
    second_factors::challenge(&api, &address).await;
    let app = reads(&api, "SELECT id::text FROM totp_factors")
        .await
        .remove(0);
    let sessions = reads(&api, "SELECT id::text FROM sessions ORDER BY id").await;
    let mut signals = api.database().signals().await;

    let removed = remove(&api, &address.to_uppercase()).await;

    let written = printed(&removed);
    assert_eq!(removed.status.code(), Some(0), "{written}");
    assert!(
        written.contains(
            "removed the account's authenticator app and recovery codes, and ended every session \
             of it; the account signs in with its password alone, and its address is told"
        ),
        "{written}"
    );
    assert!(!written.contains(&address), "{written}");
    for table in [
        "totp_factors",
        "recovery_codes",
        "sign_in_challenges",
        "sessions",
    ] {
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
        [format!("{ada_id} - second_factor {app}")]
    );
    let mut ended = entries(&api, "session.ended").await;
    ended.sort();
    let mut expected: Vec<String> = sessions
        .iter()
        .map(|session| format!("{ada_id} - session {session}"))
        .collect();
    expected.sort();
    assert_eq!(ended, expected);
    // Each ended session's event goes to the user's streams (AUTH-046).
    assert_eq!(
        users::session_events(&api, "session.ended").await,
        sessions
            .iter()
            .map(|session| format!("{ada_id} {session}"))
            .collect::<Vec<_>>()
    );
    let mut closed: Vec<String> = signals
        .received()
        .await
        .iter()
        .map(|signal| signal["session_id"].as_str().unwrap().to_owned())
        .collect();
    closed.sort();
    assert_eq!(closed, sessions);
    assert_eq!(
        reads(
            &api,
            "SELECT count(*)::text FROM audit_entry_clients c JOIN audit_entries e \
             ON e.id = c.entry_id WHERE e.action = 'second_factor.removed' \
             AND (c.client_ip IS NOT NULL OR c.user_agent IS NOT NULL)"
        )
        .await,
        ["0"],
        "an operator's subcommand records no client"
    );
}

#[tokio::test]
async fn auth_072_the_removal_tells_the_address_and_the_account_signs_in_with_its_password_alone() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    add_app(&api, &ada).await;

    let removed = remove(&api, &address).await;

    assert_eq!(removed.status.code(), Some(0), "{}", printed(&removed));
    api.run_jobs().await;
    let emails = mailbox::to(&address).await;
    let told = emails
        .iter()
        .find(|email| email.subject == REMOVED)
        .expect("the email that tells the address");
    assert!(told.text.contains("operator removed"), "{}", told.text);
    let signed_in = users::sign_in(&api, &address, PASSWORD).await;
    assert_eq!(signed_in.status, 201, "{}", signed_in.json());
}

#[tokio::test]
async fn auth_046_the_operator_s_removal_publishes_session_ended_for_each_session_it_ends() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let other = users::new_session(&api, &address).await;
    add_app(&api, &ada).await;

    let removed = remove(&api, &address).await;

    assert_eq!(removed.status.code(), Some(0), "{}", printed(&removed));
    let mut ended = vec![
        format!("{} {}", ada.user_id, ada.session_id),
        format!("{} {}", ada.user_id, other.session_id),
    ];
    ended.sort();
    assert_eq!(users::session_events(&api, "session.ended").await, ended);
}

/// The user of each email job whose payload names `subject`, `-` for no one.
async fn email_jobs(api: &Api, subject: &str) -> Vec<String> {
    reads(
        api,
        &format!(
            "SELECT coalesce(user_id::text, '-') FROM jobs WHERE kind = 'platform.send_email' \
             AND payload::text LIKE '%{subject}%'"
        ),
    )
    .await
}

/// The subject of the email that tells an address the operator removed its account's app.
const REMOVED: &str = "The authenticator app of your AntWorker account was removed";

#[tokio::test]
async fn auth_068_the_operator_s_removal_tells_the_address_that_the_operator_removed_the_app() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    add_app(&api, &ada).await;

    let removed = remove(&api, &address).await;

    assert_eq!(removed.status.code(), Some(0), "{}", printed(&removed));
    assert_eq!(email_jobs(&api, REMOVED).await.len(), 1);
    api.run_jobs().await;
    let emails = mailbox::to(&address).await;
    let told = emails
        .iter()
        .find(|email| email.subject == REMOVED)
        .expect("the email that tells the address");
    for part in [&told.text, &told.html] {
        assert!(
            part.contains("operator removed the authenticator app"),
            "{part}"
        );
        assert!(part.contains("ended every session"), "{part}");
        assert!(!part.contains("http"), "{part}");
    }
}

#[tokio::test]
async fn platform_013_the_operator_s_removal_enqueues_its_email_for_the_account_it_names() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    add_app(&api, &ada).await;

    let removed = remove(&api, &address).await;

    assert_eq!(removed.status.code(), Some(0), "{}", printed(&removed));
    assert_eq!(email_jobs(&api, REMOVED).await, [ada.user_id]);
}

#[tokio::test]
async fn auth_072_an_unknown_address_or_an_account_without_an_app_changes_nothing_and_exits_1() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register(&api, &address).await;
    let nobody = mailbox::address("nobody");
    let before = reads(&api, "SELECT id::text FROM audit_entries ORDER BY id").await;

    let unknown = remove(&api, &nobody).await;
    let no_app = remove(&api, &address).await;
    let malformed = remove(&api, "not an address").await;

    refused(
        &unknown,
        "no account holds the address: nothing changed",
        &nobody,
    );
    refused(
        &no_app,
        "the account that holds the address has no authenticator app: nothing changed",
        &address,
    );
    refused(
        &malformed,
        "the address given isn't an email address, so no account holds it: nothing changed",
        "not an address",
    );
    assert_eq!(
        reads(&api, "SELECT id::text FROM audit_entries ORDER BY id").await,
        before
    );
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM sessions").await,
        ["1"]
    );
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
async fn auth_068_with_email_off_the_operator_s_removal_enqueues_no_email_and_says_so() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    let emails = "SELECT count(*)::text FROM jobs WHERE kind = 'platform.send_email'";
    let before = reads(&api, emails).await;

    let removed = api
        .database()
        .run_api(
            &["remove-second-factor", "ada@example.com"],
            &[("ENVIRONMENT", "test"), ("EMAIL", "off")],
        )
        .await;

    let written = printed(&removed);
    assert_eq!(removed.status.code(), Some(0), "{written}");
    assert!(
        written.contains("email is off, so its address isn't told"),
        "{written}"
    );
    assert!(
        reads(&api, "SELECT id::text FROM totp_factors")
            .await
            .is_empty()
    );
    assert_eq!(reads(&api, emails).await, before);
}
