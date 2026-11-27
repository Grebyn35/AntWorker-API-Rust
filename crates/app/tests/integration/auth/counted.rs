//! How `auth`'s checks of a password, a code and a provider's code end, as its counters count
//! them (AUTH-104, AUTH-105), in a `serve` of the test's own, which exports to the stand-in
//! collector and is read once it has stopped (TEST-07). The test's API arranges the accounts,
//! their codes and their apps on the same database, and what its own requests count reaches no
//! collector.

use platform::telemetry::testing::collector::Collector;
use serde_json::{Value, json};

use crate::support::{
    Api, Server, TestDatabase, api, counts, email_change, mailbox, password_reset,
    providers::{self, gives, grant, identity},
    second_factors::{self, other_code},
    upstream::Upstream,
    users::{self, PASSWORD},
    verification, with_fields,
};

/// A code of the 16 characters of a reset code or a change code that no account was sent
/// (AUTH-036, AUTH-050).
const WRONG_LONG_CODE: &str = "0000-0000-0000-0000";

/// A recovery code of the form that AUTH-062 reads, which no account holds.
const NO_ONES_RECOVERY_CODE: &str = "7KQM-2XWD-9RTB-H4NP-1100-ABCD";

/// A password that AUTH-003 allows, for a reset or a change of the password.
const NEW_PASSWORD: &str = "lantern-orchard-58-brisk";

/// Starts `serve` on `db` in the tests' environment, with the tests' keys, so that it reads the
/// codes and the apps that a test's API issued (SEC-03, AUTH-066), exporting its telemetry to
/// `collector`, and with `env` in place of any of those settings that it names.
async fn serving(db: &TestDatabase, collector: &Collector, env: &[(&str, &str)]) -> Server {
    let endpoint = collector.endpoint();
    let mut settings = vec![
        ("ENVIRONMENT", "test"),
        ("HMAC_KEY", api::HMAC_KEY),
        ("ENCRYPTION_KEYS", api::ENCRYPTION_KEYS),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.as_str()),
    ];
    settings.extend_from_slice(env);
    db.serve(&settings).await
}

/// Posts `body` to `path` of `server`, with `access` as its bearer token when given, and gives
/// the answer's status.
async fn post(server: &Server, path: &str, access: Option<&str>, body: &Value) -> u16 {
    let bearer = access.map(|access| format!("Bearer {access}"));
    let mut headers = vec![("content-type", "application/json")];
    if let Some(bearer) = &bearer {
        headers.push(("authorization", bearer.as_str()));
    }
    let body = body.to_string();
    server.send("POST", path, &headers, body.as_bytes()).await.0
}

/// The counts of the counter `name` that `collector` received from a `serve` that has stopped,
/// by outcome, such as `succeeded 1`.
fn counted(collector: &Collector, name: &str) -> Vec<String> {
    counts::of(&collector.received(), name, "auth.outcome")
}

#[tokio::test]
async fn auth_104_sign_ins_with_a_password_are_counted_by_how_they_ended() {
    let api = Api::new().await;
    for email in ["ada@example.com", "eve@example.com"] {
        users::register(&api, email).await;
    }
    let bob = users::register(&api, "bob@example.com").await;
    second_factors::add_app(&api, &bob).await;
    let collector = Collector::start().await.unwrap();
    // One attempt of each email from an address, so that Eve's second is refused (AUTH-006).
    let limits = [("SIGN_IN_ATTEMPTS", "1"), ("SIGN_IN_EMAIL_ATTEMPTS", "2")];
    let server = serving(api.database(), &collector, &limits).await;

    // Ada's sign-in without a password, which its body's validation refuses (AUTH-004), and
    // which counts nowhere.
    let mut statuses = Vec::new();
    for (email, password) in [
        ("ada@example.com", ""),
        ("ada@example.com", PASSWORD),
        ("bob@example.com", PASSWORD),
        ("eve@example.com", "wrong-password-123"),
        ("eve@example.com", PASSWORD),
    ] {
        let body = json!({ "email": email, "password": password });
        statuses.push(post(&server, "/v1/sessions", None, &body).await);
    }
    server.stop().await;

    assert_eq!(statuses, [400, 201, 200, 401, 429]);
    assert_eq!(
        counted(&collector, "auth.sign_ins"),
        [
            "challenged 1",
            "invalid_credentials 1",
            "rate_limited 1",
            "succeeded 1"
        ]
    );
}

#[tokio::test]
async fn auth_104_exchanges_of_a_provider_s_code_are_counted_by_provider_and_by_how_they_ended() {
    let upstream = Upstream::start().await;
    let (collector, db) = (
        Collector::start().await.unwrap(),
        TestDatabase::migrated().await,
    );
    let options = providers::stand_in(&upstream);
    let server = serving(&db, &collector, &options.env()).await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));

    // The other stand-in, which is off, a provider that the API doesn't have, a redirect URI
    // that no client of the stand-in lists, and a verifier that the body's validation refuses
    // (AUTH-100): none of them makes an exchange, and none counts.
    let mut refused = Vec::new();
    for fields in [
        json!({ "provider": "stand_in_other" }),
        json!({ "provider": "nobody" }),
        json!({ "redirect_uri": "https://evil.example/callback" }),
        json!({ "code_verifier": "too-short" }),
    ] {
        let body = with_fields(grant(), fields);
        refused.push(post(&server, "/v1/sessions/provider", None, &body).await);
    }
    let succeeded = post(&server, "/v1/sessions/provider", None, &grant()).await;
    upstream.set("POST", "/token", 400, json!({ "error": "invalid_grant" }));
    let rejected = post(&server, "/v1/sessions/provider", None, &grant()).await;
    upstream.set("POST", "/token", 500, json!({}));
    let unavailable = post(&server, "/v1/sessions/provider", None, &grant()).await;
    server.stop().await;

    assert_eq!(refused, [503, 404, 400, 400]);
    assert_eq!([succeeded, rejected, unavailable], [201, 401, 503]);
    assert_eq!(
        counted(&collector, "auth.provider_exchanges"),
        ["rejected 1", "succeeded 1", "unavailable 1"]
    );
    assert_eq!(
        counts::of(
            &collector.received(),
            "auth.provider_exchanges",
            "auth.provider"
        ),
        ["stand_in 3"]
    );
}

#[tokio::test]
async fn auth_104_exchanges_at_a_link_and_a_re_authentication_are_counted_as_at_a_sign_in() {
    let upstream = Upstream::start().await;
    let api = Api::with(providers::stand_in(&upstream)).await;
    // Registered just now, so that a link finds her session's proof recent (AUTH-087).
    let ada = users::register(&api, "ada@example.com").await;
    let collector = Collector::start().await.unwrap();
    let options = providers::stand_in(&upstream);
    let server = serving(api.database(), &collector, &options.env()).await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let adas = Some(ada.access.as_str());

    // The link's exchange gives the identity, and the provider refuses the re-authentication's
    // code.
    let linked = post(&server, "/v1/users/me/identities/provider", adas, &grant()).await;
    upstream.set("POST", "/token", 400, json!({ "error": "invalid_grant" }));
    let reauthentication = "/v1/users/me/sessions/current/reauthenticate/provider";
    let reauthenticated = post(&server, reauthentication, adas, &grant()).await;
    server.stop().await;

    assert_eq!([linked, reauthenticated], [200, 403]);
    assert_eq!(
        counted(&collector, "auth.provider_exchanges"),
        ["rejected 1", "succeeded 1"]
    );
}

#[tokio::test]
async fn auth_105_confirmations_of_a_code_that_proves_the_address_are_counted_by_how_they_ended() {
    let api = Api::new().await;
    let addresses = [
        mailbox::address("ada"),
        mailbox::address("bob"),
        mailbox::address("eve"),
    ];
    let ada = users::register_unverified(&api, &addresses[0]).await;
    let bob = users::register_unverified(&api, &addresses[1]).await;
    let eve = users::register_unverified(&api, &addresses[2]).await;
    // The jobs of the three first codes run here, so that the server runs none.
    let code = verification::code_sent(&api, &addresses[0]).await;
    // Bob holds no code that lasts.
    api.database()
        .owner_executes(&format!(
            "DELETE FROM verification_codes WHERE user_id = '{}'",
            bob.user_id
        ))
        .await;
    // Ten checks of Eve's, the limit of a day (AUTH-033), through the test's API.
    for _ in 0..10 {
        verification::confirm(&api, &eve, "000000").await;
    }
    let collector = Collector::start().await.unwrap();
    let server = serving(api.database(), &collector, &[]).await;
    let path = "/v1/users/me/email/verification/confirm";

    // A code of 5 digits, which the body's validation refuses (AUTH-032), and which counts
    // nowhere.
    let short = json!({ "code": &code[1..] });
    let refused = post(&server, path, Some(&ada.access), &short).await;
    let wrong = json!({ "code": other_code(&code) });
    let wrong = post(&server, path, Some(&ada.access), &wrong).await;
    let verified = post(&server, path, Some(&ada.access), &json!({ "code": code })).await;
    let expired = post(&server, path, Some(&bob.access), &json!({ "code": code })).await;
    let limited = post(&server, path, Some(&eve.access), &json!({ "code": code })).await;
    server.stop().await;

    assert_eq!(
        [refused, wrong, verified, expired, limited],
        [400, 422, 200, 422, 429]
    );
    assert_eq!(
        counted(&collector, "auth.email_verifications"),
        [
            "code_expired 1",
            "rate_limited 1",
            "verified 1",
            "wrong_code 1"
        ]
    );
}

#[tokio::test]
async fn auth_105_confirmations_of_a_reset_are_counted_by_how_they_ended() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register(&api, &address).await;
    assert_eq!(password_reset::ask(&api, &address).await.status, 202);
    let code = password_reset::code_sent(&api, &address).await;
    let collector = Collector::start().await.unwrap();
    let server = serving(api.database(), &collector, &[]).await;
    let confirmation =
        |code: &str| json!({ "email": address, "code": code, "password": NEW_PASSWORD });

    // A code of 8 characters, which the body's validation refuses (AUTH-038), and which counts
    // nowhere. Then four wrong codes and the code fill the 5 confirmations of the email from one
    // address (AUTH-039), which refuse the sixth.
    let mut statuses = Vec::new();
    for code in ["0000-0000"]
        .into_iter()
        .chain([WRONG_LONG_CODE; 4])
        .chain([code.as_str(), WRONG_LONG_CODE])
    {
        let body = confirmation(code);
        statuses.push(post(&server, "/v1/password-resets/confirm", None, &body).await);
    }
    server.stop().await;

    assert_eq!(statuses, [400, 401, 401, 401, 401, 204, 429]);
    assert_eq!(
        counted(&collector, "auth.password_resets"),
        ["invalid_reset_code 4", "rate_limited 1", "reset 1"]
    );
}

#[tokio::test]
async fn auth_105_a_reset_that_the_second_factor_s_limit_refuses_counts_as_rate_limited() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let app = second_factors::add_app(&api, &ada).await;
    let challenge = second_factors::challenge(&api, &address).await;
    let code = second_factors::code(&api, &app.secret).await;
    // Ten wrong completions of Ada's sign-in, through the test's API, fill her bucket of
    // completions, in which a reset's second factor counts too (AUTH-061, AUTH-098).
    for _ in 0..10 {
        second_factors::complete(&api, &challenge, json!({ "code": other_code(&code) })).await;
    }
    assert_eq!(password_reset::ask(&api, &address).await.status, 202);
    let reset_code = password_reset::code_sent(&api, &address).await;
    let collector = Collector::start().await.unwrap();
    let server = serving(api.database(), &collector, &[]).await;
    let reset = json!({
        "email": address,
        "code": reset_code,
        "password": NEW_PASSWORD,
        "second_factor_code": code,
    });

    let limited = post(&server, "/v1/password-resets/confirm", None, &reset).await;
    server.stop().await;

    assert_eq!(limited, 429);
    assert_eq!(
        counted(&collector, "auth.password_resets"),
        ["rate_limited 1"]
    );
    assert_eq!(
        counted(&collector, "auth.second_factor_checks"),
        ["rate_limited 1"]
    );
}

#[tokio::test]
async fn auth_105_confirmations_of_a_change_of_the_address_are_counted_by_how_they_ended() {
    let api = Api::new().await;
    let ada = users::register(&api, &mailbox::address("ada")).await;
    let bob = users::register(&api, &mailbox::address("bob")).await;
    let carol = users::register(&api, &mailbox::address("carol")).await;
    let eve = users::register(&api, &mailbox::address("eve")).await;
    let new = ["ada-new", "bob-new", "taken", "eve-new"].map(mailbox::address);
    for (user, address) in [(&ada, &new[0]), (&carol, &new[2])] {
        assert_eq!(email_change::ask(&api, user, address).await.status, 202);
    }
    let code = email_change::code_sent(&api, &new[0]).await;
    let carols = email_change::code_sent(&api, &new[2]).await;
    // Another account takes the address that Carol's code was sent to.
    users::register(&api, &new[2]).await;
    // Ten checks of Eve's codes, the limit of a day (AUTH-033), through the test's API.
    for _ in 0..10 {
        email_change::confirm(&api, &eve, &new[3], WRONG_LONG_CODE).await;
    }
    let collector = Collector::start().await.unwrap();
    let server = serving(api.database(), &collector, &[]).await;
    let path = "/v1/users/me/email/change/confirm";
    let body = |email: &str, code: &str| json!({ "email": email, "code": code });

    // A code of 8 characters, which the body's validation refuses (AUTH-052), and which counts
    // nowhere.
    let short = body(&new[0], "0000-0000");
    let refused = post(&server, path, Some(&ada.access), &short).await;
    let wrong = post(
        &server,
        path,
        Some(&ada.access),
        &body(&new[0], WRONG_LONG_CODE),
    )
    .await;
    let changed = post(&server, path, Some(&ada.access), &body(&new[0], &code)).await;
    let expired = post(&server, path, Some(&bob.access), &body(&new[1], &code)).await;
    let in_use = post(&server, path, Some(&carol.access), &body(&new[2], &carols)).await;
    let limited = post(&server, path, Some(&eve.access), &body(&new[3], &code)).await;
    server.stop().await;

    assert_eq!(
        [refused, wrong, changed, expired, in_use, limited],
        [400, 422, 200, 422, 409, 429]
    );
    assert_eq!(
        counted(&collector, "auth.email_changes"),
        [
            "changed 1",
            "code_expired 1",
            "email_in_use 1",
            "rate_limited 1",
            "wrong_code 1"
        ]
    );
}

#[tokio::test]
async fn auth_104_checks_of_a_second_factor_are_counted_by_how_they_ended() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = second_factors::add_app(&api, &ada).await;
    let challenge = second_factors::challenge(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let setup = second_factors::start_setup(&api, &bob).await;
    assert_eq!(setup.status, 200, "{}", setup.json());
    let secret = setup.json()["secret"].as_str().unwrap().to_owned();
    let wrong = other_code(&second_factors::code(&api, &secret).await);
    // Nine of the ten checks of a day of Bob's (AUTH-061), through the test's API.
    for _ in 0..9 {
        second_factors::confirm(&api, &bob, &wrong).await;
    }
    let code = second_factors::code(&api, &app.secret).await;
    let collector = Collector::start().await.unwrap();
    let server = serving(api.database(), &collector, &[]).await;
    let completion = |code: &str| json!({ "challenge": challenge, "code": code });
    let (completing, confirming) = (
        "/v1/sessions/second-factor",
        "/v1/users/me/second-factors/totp/setup/confirm",
    );
    let bobs = Some(bob.access.as_str());

    // A completion's two checks, then Bob's tenth check of a setup, and his eleventh.
    let statuses = [
        post(&server, completing, None, &completion(&other_code(&code))).await,
        post(&server, completing, None, &completion(&code)).await,
        post(&server, confirming, bobs, &json!({ "code": wrong })).await,
        post(&server, confirming, bobs, &json!({ "code": wrong })).await,
    ];
    server.stop().await;

    assert_eq!(statuses, [401, 201, 422, 429]);
    assert_eq!(
        counted(&collector, "auth.second_factor_checks"),
        ["passed 1", "rate_limited 1", "wrong_code 2"]
    );
}

#[tokio::test]
async fn auth_104_checks_at_a_change_of_the_password_a_re_authentication_and_a_reset_are_counted() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let app = second_factors::add_app(&api, &ada).await;
    assert_eq!(password_reset::ask(&api, &address).await.status, 202);
    let reset_code = password_reset::code_sent(&api, &address).await;
    let code = second_factors::code(&api, &app.secret).await;
    let collector = Collector::start().await.unwrap();
    let server = serving(api.database(), &collector, &[]).await;
    let adas = Some(ada.access.as_str());
    let change = json!({
        "current_password": PASSWORD,
        "password": NEW_PASSWORD,
        "recovery_code": NO_ONES_RECOVERY_CODE,
    });
    let reauthentication = json!({ "password": PASSWORD, "code": code });
    let reset = json!({
        "email": address,
        "code": reset_code,
        "password": NEW_PASSWORD,
        "recovery_code": app.recovery_codes[0],
    });

    // A change of the password with a recovery code that isn't Ada's, a re-authentication with
    // the app's code, and, last, since it ends her sessions, a reset with one of her recovery
    // codes.
    let statuses = [
        post(&server, "/v1/users/me/password/change", adas, &change).await,
        post(
            &server,
            "/v1/users/me/sessions/current/reauthenticate",
            adas,
            &reauthentication,
        )
        .await,
        post(&server, "/v1/password-resets/confirm", None, &reset).await,
    ];
    server.stop().await;

    assert_eq!(statuses, [403, 200, 204]);
    assert_eq!(
        counted(&collector, "auth.second_factor_checks"),
        ["passed 2", "wrong_code 1"]
    );
}

#[tokio::test]
async fn auth_104_checks_at_a_re_authentication_through_a_provider_are_counted() {
    let (upstream, api) = providers::standing().await;
    // Registered just now, so that a link finds her session's proof recent (AUTH-087).
    let ada = users::register(&api, "ada@example.com").await;
    let app = second_factors::add_app(&api, &ada).await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let linked = providers::link(&api, &ada, &grant()).await;
    assert_eq!(linked.status, 200, "{}", linked.json());
    let wrong = other_code(&second_factors::code(&api, &app.secret).await);
    let collector = Collector::start().await.unwrap();
    let options = providers::stand_in(&upstream);
    let server = serving(api.database(), &collector, &options.env()).await;
    let path = "/v1/users/me/sessions/current/reauthenticate/provider";
    let body = with_fields(grant(), json!({ "second_factor_code": wrong }));

    let reauthenticated = post(&server, path, Some(&ada.access), &body).await;
    server.stop().await;

    assert_eq!(reauthenticated, 403);
    assert_eq!(
        counted(&collector, "auth.second_factor_checks"),
        ["wrong_code 1"]
    );
}

#[tokio::test]
async fn auth_104_a_second_factor_s_code_that_is_missing_unchecked_or_refused_counts_nowhere() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = second_factors::add_app(&api, &ada).await;
    let code = second_factors::code(&api, &app.secret).await;
    let collector = Collector::start().await.unwrap();
    let server = serving(api.database(), &collector, &[]).await;
    let path = "/v1/users/me/sessions/current/reauthenticate";
    let adas = Some(ada.access.as_str());

    // No code, which an account with an app needs, the app's code beside a wrong password, which
    // leaves the code unchecked, and a code of 5 digits, which the body's validation refuses
    // (AUTH-065): none of them counts. Then the app's code.
    let mut statuses = Vec::new();
    for body in [
        json!({ "password": PASSWORD }),
        json!({ "password": "wrong-password-123", "code": code }),
        json!({ "password": PASSWORD, "code": &code[1..] }),
        json!({ "password": PASSWORD, "code": code }),
    ] {
        statuses.push(post(&server, path, adas, &body).await);
    }
    server.stop().await;

    assert_eq!(statuses, [403, 403, 400, 200]);
    assert_eq!(
        counted(&collector, "auth.second_factor_checks"),
        ["passed 1"]
    );
}
