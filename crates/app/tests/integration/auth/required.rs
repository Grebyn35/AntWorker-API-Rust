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

/// What `output` wrote to stdout and stderr.
fn printed(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
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
async fn auth_070_the_setting_is_true_or_false() {
    let refusal = api::refusal(&Options::default().set("SECOND_FACTOR_REQUIRED", "yes"));

    assert_eq!(refusal, "SECOND_FACTOR_REQUIRED must be true or false");
}

#[tokio::test]
async fn auth_071_with_the_setting_on_removing_the_app_answers_409_and_changes_nothing() {
    let api = requiring().await;
    let ada = users::register(&api, "ada@example.com").await;
    add_app(&api, &ada).await;
    let before = reads(&api, "SELECT id::text FROM totp_factors").await;

    let refused = api
        .request(http::Method::DELETE, "/v1/users/me/second-factors/totp")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(refused.status, 409);
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
async fn auth_072_the_subcommand_removes_the_app_and_ends_the_sessions_and_tells_the_address() {
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

    let (stdout, stderr) = printed(&removed);
    assert_eq!(removed.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.contains(&format!(
            "Removed the authenticator app and the recovery codes of {address}"
        )),
        "{stdout}"
    );
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
    api.run_jobs().await;
    let emails = mailbox::to(&address).await;
    let told = emails
        .iter()
        .find(|email| {
            email.subject == "The authenticator app of your AntWorker account was removed"
        })
        .expect("the email that tells the address");
    assert!(told.text.contains("operator removed"), "{}", told.text);
    // The account signs in with its password alone.
    let signed_in = users::sign_in(&api, &address, PASSWORD).await;
    assert_eq!(signed_in.status, 201, "{}", signed_in.json());
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

    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        printed(&unknown)
            .1
            .contains(&format!("No account holds {nobody}: nothing changed.")),
        "{:?}",
        printed(&unknown)
    );
    assert_eq!(no_app.status.code(), Some(1));
    assert!(
        printed(&no_app).1.contains(&format!(
            "The account of {address} has no authenticator app: nothing changed."
        )),
        "{:?}",
        printed(&no_app)
    );
    assert_eq!(malformed.status.code(), Some(1));
    assert!(
        printed(&malformed).1.contains("isn't an email address"),
        "{:?}",
        printed(&malformed)
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
async fn auth_072_with_email_off_the_removal_tells_no_one() {
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

    assert_eq!(removed.status.code(), Some(0), "{:?}", printed(&removed));
    assert!(
        reads(&api, "SELECT id::text FROM totp_factors")
            .await
            .is_empty()
    );
    assert_eq!(reads(&api, emails).await, before);
}
