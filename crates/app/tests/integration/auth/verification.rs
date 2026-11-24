//! Proving an account's email address with a code sent to it (the auth spec's "Email
//! verification", AUTH-025 to AUTH-033).

use serde_json::json;

use crate::support::{
    Api, Options, closed_port, execute_on, mailbox, queries_wait_on_locks, second_factors,
    users::{self, SignedIn},
    verification::{ask, code_sent, confirm},
    workspaces,
};

/// A code of 6 digits that isn't `code`.
fn other_than(code: &str) -> String {
    let first = code.as_bytes()[0];
    let replaced = if first == b'9' {
        '0'
    } else {
        char::from(first + 1)
    };
    format!("{replaced}{}", &code[1..])
}

#[tokio::test]
async fn auth_025_an_account_that_must_verify_is_refused_but_where_verifying_needs() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;

    for path in ["/v1/users/me", "/v1/users/me/exports", "/v1/workspaces"] {
        let refused = api.get(path).bearer(&ada.access).send().await;
        assert_eq!(refused.status, 403, "{path}");
        assert_eq!(refused.json()["code"], "request.email_unverified");
    }
    let renamed = users::rename(&api, &ada, "Ada", None).await;
    assert_eq!(renamed.status, 403);
    assert_eq!(renamed.json()["code"], "request.email_unverified");

    let session = users::current(&api, &ada.access).await;
    assert_eq!(session.status, 200);
    // Re-authenticating serves a change of the address, which corrects a mistyped one (AUTH-048).
    let reauthenticated = users::reauthenticate(&api, &ada, users::PASSWORD).await;
    assert_eq!(reauthenticated.status, 200);
    // With the tokens that the proof gave the session (AUTH-095).
    let ada = users::SignedIn::from(&reauthenticated);
    assert_eq!(ask(&api, &ada).await.status, 202);
    let code = code_sent(&api, &address).await;
    assert_eq!(confirm(&api, &ada, &code).await.status, 200);

    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(me.status, 200);
    assert_eq!(me.json()["email_verified"], true);
    let signed_out = users::sign_out(&api).bearer(&ada.access).send().await;
    assert_eq!(signed_out.status, 204);
}

/// The eight routes that verifying needs, each as its method and its path (AUTH-025).
const VERIFYING: [(&str, &str); 8] = [
    ("POST", "/v1/users/me/email/verification"),
    ("POST", "/v1/users/me/email/verification/confirm"),
    ("GET", "/v1/users/me/sessions/current"),
    ("DELETE", "/v1/users/me/sessions/current"),
    ("POST", "/v1/users/me/sessions/current/reauthenticate"),
    (
        "POST",
        "/v1/users/me/sessions/current/reauthenticate/provider",
    ),
    ("POST", "/v1/users/me/email/change"),
    ("POST", "/v1/users/me/email/change/confirm"),
];

#[tokio::test]
async fn auth_025_every_route_that_isn_t_public_refuses_an_account_that_must_verify_but_the_eight()
{
    let api = Api::new().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;
    let mut operations = api.operations();
    // Signing out ends the session that the other requests carry, so it goes last.
    operations.sort_by_key(|(method, path, _)| {
        *method == http::Method::DELETE && path == "/v1/users/me/sessions/current"
    });
    let (mut refused, mut others) = (0, Vec::new());

    for (method, path, access) in operations {
        if access == "public" {
            continue;
        }
        let response = api
            .request(method.clone(), &path)
            .header("content-type", "application/json")
            .body("{}")
            .bearer(&ada.access)
            .send()
            .await;
        if response.status == 403 && response.json()["code"] == "request.email_unverified" {
            refused += 1;
        } else {
            others.push((method.to_string(), path));
        }
    }

    others.sort();
    let mut verifying = VERIFYING.map(|(method, path)| (method.to_owned(), path.to_owned()));
    verifying.sort();
    assert_eq!(others, verifying);
    assert!(refused >= 7, "only {refused} routes refused");
}

#[tokio::test]
async fn auth_025_a_member_whose_account_must_verify_is_refused_on_a_workspace_route() {
    let api = Api::new().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;
    let acme = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;

    let refused = workspaces::read(&api, &ada, &acme).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.email_unverified");
    users::verified(&api, &ada.user_id).await;
    let read = workspaces::read(&api, &ada, &acme).await;
    assert_eq!(read.status, 200);
}

#[tokio::test]
async fn auth_025_a_session_that_a_sign_in_started_is_refused_alike() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register_unverified(&api, &address).await;
    let signed_in = users::new_session(&api, &address).await;

    let refused = api
        .get("/v1/users/me")
        .bearer(&signed_in.access)
        .send()
        .await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.email_unverified");
}

#[tokio::test]
async fn auth_025_an_account_that_must_verify_signs_out() {
    let api = Api::new().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;

    let signed_out = users::sign_out(&api).bearer(&ada.access).send().await;

    assert_eq!(signed_out.status, 204);
}

#[tokio::test]
async fn auth_025_with_email_off_no_request_is_refused_for_the_address() {
    let api = Api::without_email().await;
    let ada = users::register_unverified(&api, "ada@example.com").await;

    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(me.status, 200);
    assert_eq!(me.json()["email_verified"], false);
}

#[tokio::test]
async fn auth_025_an_account_that_registered_while_email_was_off_must_verify_once_it_is_on() {
    let off = Api::without_email().await;
    let ada = users::register_unverified(&off, &mailbox::address("ada")).await;
    let on = off.instance(Options::default()).await;

    let refused = on.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.email_unverified");
}

#[tokio::test]
async fn auth_026_the_session_says_whether_its_account_must_still_verify() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let registered = api
        .post("/v1/users", &users::registration(&address))
        .send()
        .await;
    assert_eq!(registered.json()["email_verification_required"], true);
    let ada = SignedIn::from(&registered);
    let signed_in = users::sign_in(&api, &address, users::PASSWORD).await;
    assert_eq!(signed_in.json()["email_verification_required"], true);
    let refreshed = users::refresh(&api, &ada.refresh).await;
    assert_eq!(refreshed.json()["email_verification_required"], true);
    let ada = SignedIn::from(&refreshed);
    let current = users::current(&api, &ada.access).await;
    assert_eq!(current.json()["email_verification_required"], true);

    let code = code_sent(&api, &address).await;
    let confirmed = confirm(&api, &ada, &code).await;

    assert_eq!(confirmed.json()["email_verification_required"], false);
    let current = users::current(&api, &ada.access).await;
    assert_eq!(current.json()["email_verification_required"], false);
    let signed_in = users::sign_in(&api, &address, users::PASSWORD).await;
    assert_eq!(signed_in.json()["email_verification_required"], false);
}

#[tokio::test]
async fn auth_026_with_email_off_no_session_must_verify() {
    let api = Api::without_email().await;

    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;

    assert_eq!(registered.status, 201);
    assert_eq!(registered.json()["email_verification_required"], false);
}

#[tokio::test]
async fn auth_026_a_completion_of_a_sign_in_says_whether_the_account_must_prove_its_address() {
    // An account that set up its app while email was off, which must prove its address once a
    // deploy turns email on (AUTH-025).
    let off = Api::without_email().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&off, &address).await;
    let app = second_factors::add_app(&off, &ada).await;
    let on = off.instance(Options::default()).await;
    let complete = |api: &Api| {
        let (api, secret, user_id) = (api.clone(), app.secret.clone(), ada.user_id.clone());
        let address = address.clone();
        async move {
            second_factors::forget_steps(&api, &user_id).await;
            let challenge = second_factors::challenge(&api, &address).await;
            let code = second_factors::code(&api, &secret).await;
            second_factors::complete(&api, &challenge, json!({ "code": code })).await
        }
    };

    let email_off = complete(&off).await;
    let unproved = complete(&on).await;
    users::verified(&on, &ada.user_id).await;
    let proved = complete(&on).await;

    for (response, required, what) in [
        (&email_off, false, "with email off"),
        (&unproved, true, "with email on, unproved"),
        (&proved, false, "with email on, proved"),
    ] {
        assert_eq!(response.status, 201, "{what}: {}", response.json());
        assert_eq!(
            response.json()["email_verification_required"],
            required,
            "{what}"
        );
    }
}

#[tokio::test]
async fn auth_026_a_change_of_the_password_says_whether_the_account_must_prove_its_address() {
    // With email on, only an account that proved its address may change its password (AUTH-025).
    let on = Api::new().await;
    let ada = users::register(&on, &mailbox::address("ada")).await;
    let off = Api::without_email().await;
    let grace = users::register_unverified(&off, "grace@example.com").await;
    let new = "walnut-harbor-77-brisk";

    let proved = users::change_password(&on, &ada, users::PASSWORD, new).await;
    let email_off = users::change_password(&off, &grace, users::PASSWORD, new).await;

    for (response, what) in [
        (&proved, "with email on, proved"),
        (&email_off, "with email off"),
    ] {
        assert_eq!(response.status, 200, "{what}: {}", response.json());
        assert_eq!(
            response.json()["email_verification_required"],
            false,
            "{what}"
        );
    }
    assert_eq!(
        off.database()
            .owner_reads("SELECT email_verified::text FROM users")
            .await,
        ["false"],
        "Grace's address isn't proved"
    );
}

#[tokio::test]
async fn auth_027_registering_enqueues_the_job_of_the_first_code() {
    let api = Api::new().await;

    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;

    assert_eq!(
        api.database()
            .owner_reads("SELECT kind || ' ' || user_id || ' ' || payload::text FROM jobs")
            .await,
        [format!(
            "auth.send_verification_code {} {{\"purpose\": \"email_verification\"}}",
            ada.user_id
        )]
    );
}

#[tokio::test]
async fn auth_027_with_email_off_registering_enqueues_nothing() {
    let api = Api::without_email().await;

    users::register_unverified(&api, "ada@example.com").await;

    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_028_asking_for_a_code_enqueues_its_job_and_names_the_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    api.run_jobs().await;

    let asked = ask(&api, &ada).await;

    assert_eq!(asked.status, 202);
    assert_eq!(asked.json(), json!({ "email": address }));
    assert_eq!(
        api.database()
            .owner_reads("SELECT kind || ' ' || user_id FROM jobs")
            .await,
        [format!("auth.send_verification_code {}", ada.user_id)]
    );
    let first = code_sent(&api, &address).await;
    assert_eq!(mailbox::to(&address).await.len(), 2);
    assert_eq!(confirm(&api, &ada, &first).await.status, 200);
}

#[tokio::test]
async fn auth_028_an_account_that_proved_its_address_answers_409() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;

    let asked = ask(&api, &ada).await;

    assert_eq!(asked.status, 409);
    assert_eq!(asked.json()["code"], "auth.email_already_verified");
    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_028_an_account_that_proved_its_address_while_the_request_waited_answers_409() {
    let api = Api::new().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;
    let db = api.database();
    // The registration enqueued a code's job.
    db.owner_executes("DELETE FROM jobs").await;
    // A confirmation of a code that holds the user's row until it commits, which proves the
    // address once the request has read that it isn't proved.
    let mut confirmation = db
        .holding(&format!(
            "UPDATE users SET email_verified = true WHERE id = '{}'",
            ada.user_id
        ))
        .await;

    let asked = api.spawn(
        http::Method::POST,
        "/v1/users/me/email/verification",
        &ada.access,
        None,
    );
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut confirmation, "COMMIT").await;
    let (status, body) = asked.await.unwrap();

    assert_eq!(status, 409);
    assert_eq!(body["code"], "auth.email_already_verified");
    let jobs = db.owner_reads("SELECT kind FROM jobs").await;
    assert!(jobs.is_empty(), "{jobs:?}");
}

#[tokio::test]
async fn auth_028_with_email_off_asking_answers_503() {
    let api = Api::without_email().await;
    let ada = users::register_unverified(&api, "ada@example.com").await;

    let asked = ask(&api, &ada).await;

    assert_eq!(asked.status, 503);
    assert_eq!(asked.json()["code"], "request.feature_disabled");
    // Only a deploy turns the feature on again, so a client waits an hour (OPS-04).
    assert_eq!(asked.header("retry-after"), "3600");
    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_029_asking_for_codes_counts_in_a_bucket_of_the_address() {
    let api = Api::new().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;
    api.run_jobs().await;

    for _ in 0..3 {
        assert_eq!(ask(&api, &ada).await.status, 202);
    }
    let refused = ask(&api, &ada).await;

    assert_eq!(refused.status, 429);
    assert_eq!(refused.json()["code"], "request.rate_limited");
    assert!(
        refused
            .header("ratelimit-policy")
            .starts_with("\"verification_email\";q=3;w=900"),
        "{}",
        refused.header("ratelimit-policy")
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM jobs")
            .await,
        ["3"]
    );
}

#[tokio::test]
async fn auth_029_registrations_of_an_address_never_fill_the_bucket_of_its_codes() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    api.run_jobs().await;
    // Anyone who knows the address registers it until its registrations' bucket, which Ada's
    // registration counted in first, is full.
    let registration = users::registration(&address);
    for status in [409, 409, 429] {
        assert_eq!(
            api.post("/v1/users", &registration).send().await.status,
            status
        );
    }

    let asked = ask(&api, &ada).await;

    assert_eq!(asked.status, 202);
}

#[tokio::test]
async fn auth_029_registering_counts_in_the_bucket_of_its_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let registration = users::registration(&address);
    assert_eq!(
        api.post("/v1/users", &registration).send().await.status,
        201
    );
    for _ in 0..2 {
        let again = api.post("/v1/users", &registration).send().await;
        assert_eq!(again.status, 409);
    }

    let refused = api.post("/v1/users", &registration).send().await;

    assert_eq!(refused.status, 429);
    assert_eq!(refused.json()["code"], "request.rate_limited");
    assert!(
        refused
            .header("ratelimit-policy")
            .starts_with("\"registration_email\";q=3;w=900"),
        "{}",
        refused.header("ratelimit-policy")
    );
}

#[tokio::test]
async fn auth_029_with_email_off_registering_counts_in_no_bucket_of_the_address() {
    let api = Api::without_email().await;
    let registration = users::registration("ada@example.com");
    assert_eq!(
        api.post("/v1/users", &registration).send().await.status,
        201
    );

    for _ in 0..3 {
        let again = api.post("/v1/users", &registration).send().await;
        assert_eq!(again.status, 409);
    }
}

#[tokio::test]
async fn auth_030_the_job_stores_only_the_code_s_hmac_and_sends_the_code() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;

    let code = code_sent(&api, &address).await;

    assert_eq!(code.len(), 6);
    let stored = api
        .database()
        .owner_reads(
            "SELECT purpose || ' ' || encode(code_hash, 'hex') || ' ' \
         || (expires_at - created_at)::text FROM verification_codes",
        )
        .await;
    assert_eq!(stored.len(), 1);
    let fields: Vec<&str> = stored[0].split(' ').collect();
    assert_eq!(fields[0], "email_verification");
    assert_eq!(fields[1].len(), 64);
    assert!(!fields[1].contains(&code));
    assert_eq!(fields[2], "00:10:00");
    let id = api
        .database()
        .owner_reads("SELECT id::text FROM verification_codes")
        .await
        .remove(0);
    assert_eq!(
        api.database().owner_reads("SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_type \
             || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = 'verification_code.issued'"
        )
        .await,
        [format!("{} - verification_code {id} {{}}", ada.user_id)]
    );
    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_030_a_newer_code_replaces_the_one_before_it() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    let first = code_sent(&api, &address).await;
    ask(&api, &ada).await;
    let second = code_sent(&api, &address).await;

    if first != second {
        let replaced = confirm(&api, &ada, &first).await;
        assert_eq!(replaced.status, 422);
        assert_eq!(replaced.json()["code"], "auth.wrong_code");
    }
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM verification_codes")
            .await,
        ["1"]
    );
    assert_eq!(confirm(&api, &ada, &second).await.status, 200);
}

#[tokio::test]
async fn auth_030_a_run_for_an_account_that_proved_its_address_issues_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    api.database()
        .owner_executes(&format!(
            "UPDATE users SET email_verified = true WHERE id = '{}'",
            ada.user_id
        ))
        .await;

    assert_eq!(api.run_jobs().await, 1);

    assert!(mailbox::to(&address).await.is_empty());
    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM verification_codes")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_030_a_run_that_another_run_beat_to_the_next_code_issues_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    // Another run's code, which its unit of work holds uncommitted while this run issues its
    // own: the unique constraint makes this run's insert wait, then do nothing (ERR-05).
    let mut other = api
        .database()
        .holding(&format!(
            "INSERT INTO verification_codes (user_id, purpose, code_hash, expires_at) \
             VALUES ('{}', 'email_verification', '\\x0123abcd', now() + interval '10 minutes')",
            ada.user_id
        ))
        .await;

    let run = tokio::spawn({
        let api = api.clone();
        async move { api.run_jobs().await }
    });
    crate::support::queries_wait_on_locks(api.database(), 1).await;
    crate::support::execute_on(&mut other, "COMMIT").await;

    assert_eq!(run.await.unwrap(), 1);
    assert!(mailbox::to(&address).await.is_empty());
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT encode(code_hash, 'hex') || ' ' || (SELECT count(*) FROM audit_entries \
             WHERE action = 'verification_code.issued') FROM verification_codes"
            )
            .await,
        ["0123abcd 0"]
    );
}

#[tokio::test]
async fn accounts_019_asking_for_a_code_that_waited_for_the_account_s_deletion_answers_401() {
    let api = Api::new().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;
    let db = api.database();
    // The registration enqueued a code's job.
    db.owner_executes("DELETE FROM jobs").await;
    // A deletion of the account that holds the user's row, and their sessions with it.
    let mut deletion = db
        .holding(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    let asked = api.spawn(
        http::Method::POST,
        "/v1/users/me/email/verification",
        &ada.access,
        None,
    );
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, body) = asked.await.unwrap();

    // Its turn came once the user was gone, so it enqueued no job for them.
    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    let jobs = db.owner_reads("SELECT kind FROM jobs").await;
    assert!(jobs.is_empty(), "{jobs:?}");
}

#[tokio::test]
async fn auth_030_a_run_for_an_account_that_is_gone_issues_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    // The account goes, and its job, which names its user without a foreign key, stays.
    api.database()
        .owner_executes(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    assert_eq!(api.run_jobs().await, 1);

    assert!(mailbox::to(&address).await.is_empty());
    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_030_a_run_on_a_replica_with_email_off_issues_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register_unverified(&api, &address).await;
    let off = api.instance(Options::email_off()).await;

    assert_eq!(off.run_jobs().await, 1);

    assert!(mailbox::to(&address).await.is_empty());
    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM verification_codes")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_030_an_attempt_after_a_failed_send_issues_a_new_code_in_place_of_its_own() {
    let address = mailbox::address("ada");
    let unreachable = closed_port().await;
    let failing = Api::with(Options::default().set("SMTP_PORT", unreachable.port())).await;
    users::register_unverified(&failing, &address).await;
    assert_eq!(failing.run_jobs().await, 1);
    let first = failing
        .database()
        .owner_reads("SELECT id::text FROM verification_codes")
        .await;
    assert_eq!(first.len(), 1);
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

    let second = failing
        .database()
        .owner_reads("SELECT id::text FROM verification_codes")
        .await;
    assert_eq!(second.len(), 1);
    assert_ne!(second, first);
    assert_eq!(mailbox::to(&address).await.len(), 1);
    assert_eq!(
        failing
            .database()
            .owner_reads(
                "SELECT count(*)::text FROM audit_entries WHERE action = 'verification_code.issued'"
            )
            .await,
        ["2"]
    );
}

#[tokio::test]
async fn auth_031_of_two_that_confirm_one_code_at_once_only_one_proves_it() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    let code = code_sent(&api, &address).await;
    // The first confirmation, which locks the user's row, claims the code and proves the
    // address, as a check does, held open while the second runs.
    let mut first = api
        .database()
        .holding(&format!(
            "UPDATE users SET email_verified = true WHERE id = '{0}';
            DELETE FROM verification_codes WHERE user_id = '{0}'",
            ada.user_id
        ))
        .await;

    let second = tokio::spawn({
        let api = api.clone();
        async move { confirm(&api, &ada, &code).await }
    });
    crate::support::queries_wait_on_locks(api.database(), 1).await;
    crate::support::execute_on(&mut first, "COMMIT").await;
    let answered = second.await.unwrap();

    assert_eq!(answered.status, 409);
    assert_eq!(answered.json()["code"], "auth.email_already_verified");
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM audit_entries WHERE action = 'email.verified'")
            .await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_031_a_session_that_another_request_ended_meanwhile_proves_nothing_and_answers_401() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    let code = code_sent(&api, &address).await;
    let session = ada.session_id.clone();
    // A lock on the user's row, as a deletion of the account or another check takes it first,
    // which the confirmation waits for.
    let mut held = api
        .database()
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR NO KEY UPDATE",
            ada.user_id
        ))
        .await;

    let confirmed = tokio::spawn({
        let api = api.clone();
        async move { confirm(&api, &ada, &code).await }
    });
    crate::support::queries_wait_on_locks(api.database(), 1).await;
    crate::support::execute_on(
        &mut held,
        &format!("DELETE FROM sessions WHERE id = '{session}'"),
    )
    .await;
    crate::support::execute_on(&mut held, "COMMIT").await;
    let answered = confirmed.await.unwrap();

    assert_eq!(answered.status, 401);
    assert_eq!(answered.json()["code"], "request.unauthenticated");
    assert_eq!(
        api.database()
            .owner_reads("SELECT email_verified::text || ' ' || (SELECT count(*) FROM verification_codes) FROM users")
            .await,
        ["false 1"]
    );
}

#[tokio::test]
async fn auth_031_a_confirmation_that_waited_for_the_account_s_deletion_answers_401() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    let code = code_sent(&api, &address).await;
    let mut deletion = api
        .database()
        .holding(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    let confirmed = tokio::spawn({
        let api = api.clone();
        async move { confirm(&api, &ada, &code).await }
    });
    crate::support::queries_wait_on_locks(api.database(), 1).await;
    crate::support::execute_on(&mut deletion, "COMMIT").await;
    let answered = confirmed.await.unwrap();

    assert_eq!(answered.status, 401);
    assert_eq!(answered.json()["code"], "request.unauthenticated");
}

#[tokio::test]
async fn auth_031_the_right_code_proves_the_address_once() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    let code = code_sent(&api, &address).await;

    let confirmed = confirm(&api, &ada, &format!("  {code} ")).await;

    assert_eq!(confirmed.status, 200);
    let session = confirmed.json();
    assert_eq!(session["id"], ada.session_id.as_str());
    assert_eq!(session["email_verification_required"], false);
    assert!(session.get("access_token").is_none());
    assert_eq!(
        api.database()
            .owner_reads("SELECT email_verified::text FROM users")
            .await,
        ["true"]
    );
    assert!(
        api.database()
            .owner_reads("SELECT id::text FROM verification_codes")
            .await
            .is_empty()
    );
    assert_eq!(
        api.database().owner_reads("SELECT user_id || ' ' || actor_user_id || ' ' || resource_type || ' ' || resource_id \
             || ' ' || details::text FROM audit_entries WHERE action = 'email.verified'"
        )
        .await,
        [format!("{0} {0} user {0} {{}}", ada.user_id)]
    );
    let again = confirm(&api, &ada, &code).await;
    assert_eq!(again.status, 409);
    assert_eq!(again.json()["code"], "auth.email_already_verified");
}

#[tokio::test]
async fn auth_032_a_code_that_a_newer_one_replaces_while_the_check_runs_answers_code_expired() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    let code = code_sent(&api, &address).await;
    // A newer code's job deletes the code, and holds it while this check runs, then commits,
    // as the job that replaces it does.
    let mut holder = api
        .database()
        .holding(&format!(
            "DELETE FROM verification_codes WHERE user_id = '{}'",
            ada.user_id
        ))
        .await;

    let check = tokio::spawn({
        let api = api.clone();
        async move { confirm(&api, &ada, &code).await }
    });
    crate::support::queries_wait_on_locks(api.database(), 1).await;
    crate::support::execute_on(&mut holder, "COMMIT").await;
    let answered = check.await.unwrap();

    assert_eq!(answered.status, 422);
    assert_eq!(answered.json()["code"], "auth.code_expired");
    assert_eq!(
        api.database()
            .owner_reads("SELECT email_verified::text FROM users")
            .await,
        ["false"]
    );
}

#[tokio::test]
async fn auth_032_a_wrong_code_is_recorded_and_leaves_the_code() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    let code = code_sent(&api, &address).await;

    let wrong = confirm(&api, &ada, &other_than(&code)).await;

    assert_eq!(wrong.status, 422);
    assert_eq!(wrong.json()["code"], "auth.wrong_code");
    let issued = api
        .database()
        .owner_reads("SELECT id::text FROM verification_codes")
        .await;
    assert_eq!(
        api.database().owner_reads("SELECT user_id || ' ' || actor_user_id || ' ' || resource_type || ' ' || resource_id \
             || ' ' || details::text FROM audit_entries WHERE action = 'verification_code.check_failed'"
        )
        .await,
        [format!(
            "{0} {0} verification_code {1} {{}}",
            ada.user_id, issued[0]
        )]
    );
    assert_eq!(confirm(&api, &ada, &code).await.status, 200);
}

#[tokio::test]
async fn auth_032_an_expired_code_or_none_answers_code_expired() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;

    let none = confirm(&api, &ada, "123456").await;
    assert_eq!(none.status, 422);
    assert_eq!(none.json()["code"], "auth.code_expired");

    let code = code_sent(&api, &address).await;
    api.database()
        .owner_executes(
            "UPDATE verification_codes SET created_at = now() - interval '11 minutes', \
             expires_at = now() - interval '1 minute'",
        )
        .await;
    let expired = confirm(&api, &ada, &code).await;

    assert_eq!(expired.status, 422);
    assert_eq!(expired.json()["code"], "auth.code_expired");
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT count(*)::text FROM audit_entries \
             WHERE action = 'verification_code.check_failed'"
            )
            .await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_032_a_code_of_more_than_32_characters_or_that_isn_t_6_digits_answers_400() {
    let api = Api::new().await;
    let ada = users::register_unverified(&api, &mailbox::address("ada")).await;
    // Six digits that the spaces before them take past 32 characters.
    let long = format!("{}123456", " ".repeat(27));

    for code in ["12345", "1234567", "12a456", "", long.as_str()] {
        let refused = api
            .post_raw(
                "/v1/users/me/email/verification/confirm",
                &json!({ "code": code }),
            )
            .bearer(&ada.access)
            .send()
            .await;
        assert_eq!(refused.status, 400, "{code}");
        assert_eq!(
            refused.json()["errors"],
            json!([{
                "field": "/code",
                "code": "invalid_code",
                "message": "A code is 6 digits.",
            }])
        );
    }
}

#[tokio::test]
async fn auth_032_a_code_of_32_characters_with_the_whitespace_around_its_digits_is_checked() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    // The right code, which the spaces before it take to 32 characters.
    let padded = format!("{}{}", " ".repeat(26), code_sent(&api, &address).await);

    let confirmed = confirm(&api, &ada, &padded).await;

    assert_eq!(confirmed.status, 200, "{}", confirmed.json());
    assert_eq!(
        api.database()
            .owner_reads("SELECT email_verified::text FROM users")
            .await,
        ["true"]
    );
}

#[tokio::test]
async fn auth_032_with_email_off_a_code_of_6_digits_answers_503_and_a_malformed_one_400() {
    let api = Api::without_email().await;
    let ada = users::register_unverified(&api, "ada@example.com").await;

    let refused = confirm(&api, &ada, "123456").await;
    let malformed = api
        .post_raw(
            "/v1/users/me/email/verification/confirm",
            &json!({ "code": "12a" }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(refused.status, 503);
    assert_eq!(refused.json()["code"], "request.feature_disabled");
    assert_eq!(malformed.status, 400);
    assert_eq!(malformed.json()["errors"][0]["code"], "invalid_code");
}

#[tokio::test]
async fn auth_032_an_account_that_proved_its_address_answers_409() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;

    let refused = confirm(&api, &ada, "123456").await;

    assert_eq!(refused.status, 409);
    assert_eq!(refused.json()["code"], "auth.email_already_verified");
}

#[tokio::test]
async fn auth_033_every_check_counts_in_a_bucket_of_the_user_s() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register_unverified(&api, &address).await;
    let code = code_sent(&api, &address).await;
    let wrong = other_than(&code);

    for _ in 0..10 {
        assert_eq!(confirm(&api, &ada, &wrong).await.status, 422);
    }
    let refused = confirm(&api, &ada, &code).await;

    assert_eq!(refused.status, 429);
    assert_eq!(refused.json()["code"], "request.rate_limited");
    assert!(
        refused
            .header("ratelimit-policy")
            .starts_with("\"code_check\";q=10;w=86400"),
        "{}",
        refused.header("ratelimit-policy")
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT email_verified::text FROM users")
            .await,
        ["false"]
    );
    let bob = users::register_unverified(&api, &mailbox::address("bob")).await;
    assert_eq!(confirm(&api, &bob, "123456").await.status, 422);
}
