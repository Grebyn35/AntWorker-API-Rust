//! Signing in, registering and re-authenticating through a provider, which every provider shares,
//! proven through the tests' stand-in provider (the auth spec's "Signing in through a provider").

use platform::telemetry::testing;
use serde_json::{Value, json};

use crate::support::{
    Api,
    api::{self, Options, Response},
    execute_on,
    providers::{self, VERIFIER, gives, grant, identity, sign_in, signed_in},
    queries_wait_on_locks, second_factors,
    upstream::Upstream,
    users::{self, SignedIn},
};

/// A stand-in provider, and an API that signs in through it.
async fn standing() -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let api = Api::with(providers::stand_in(&upstream)).await;
    (upstream, api)
}

/// The actions of the entries whose details name the stand-in.
async fn provider_entries(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT action FROM audit_entries WHERE details->>'provider' = 'stand_in' \
             ORDER BY created_at, id",
        )
        .await
}

#[tokio::test]
async fn auth_073_a_linked_subject_signs_in_to_its_account_whatever_address_it_gives() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let first = signed_in(&api).await;
    gives(
        &upstream,
        identity("subject-1", "ada@elsewhere.example", true),
    );

    let mut body = grant();
    body["device_id"] = json!("laptop");
    let response = sign_in(&api, &body).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(response.header("location"), "/v1/users/me/sessions/current");
    let session = response.json();
    assert_eq!(session["user_id"], first.user_id.as_str());
    assert_eq!(session["provider"], "stand_in");
    assert_eq!(session["device_id"], "laptop");
    assert!(session["access_token"].is_string());
    assert!(session["device_token"].is_string(), "AUTH-019");
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM users")
            .await,
        ["1"]
    );
    assert_eq!(
        provider_entries(&api).await,
        ["user.registered", "session.created"]
    );
    let listed = api
        .get("/v1/users/me/sessions")
        .bearer(&first.access)
        .send()
        .await;
    assert!(
        listed.json()["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["provider"] == "stand_in"),
        "AUTH-043"
    );
}

#[tokio::test]
async fn auth_073_an_account_with_an_app_gets_a_challenge_whose_session_records_the_provider() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    let app = second_factors::add_app(&api, &ada).await;

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 200, "{}", response.json());
    let challenge = response.json()["challenge"].as_str().unwrap().to_owned();
    let code = second_factors::code(&api, &app.secret).await;
    let completed = second_factors::complete(&api, &challenge, json!({ "code": code })).await;
    assert_eq!(completed.status, 201, "{}", completed.json());
    assert_eq!(completed.json()["provider"], "stand_in");
    assert!(
        provider_entries(&api)
            .await
            .contains(&"sign_in.challenged".to_owned())
    );
}

#[tokio::test]
async fn auth_074_a_new_subject_registers_with_the_provider_s_name_and_identity() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "Ada@Example.com", false));

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    let session = response.json();
    assert_eq!(session["email_verification_required"], true);
    let user = session["user_id"].as_str().unwrap();
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT email || ' ' || name || ' ' || email_verified::text FROM users \
                 WHERE id = '{user}'"
            ))
            .await,
        ["ada@example.com Ada Lovelace false"]
    );
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT provider || ' ' || subject FROM provider_identities WHERE user_id = '{user}'"
            ))
            .await,
        ["stand_in subject-1"]
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM identities")
            .await,
        ["0"],
        "no password"
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT kind FROM jobs WHERE kind = 'auth.send_verification_code'")
            .await,
        ["auth.send_verification_code"],
        "AUTH-027"
    );
}

#[tokio::test]
async fn auth_074_without_a_name_the_address_s_part_before_its_at_names_the_account() {
    let (upstream, api) = standing().await;
    gives(
        &upstream,
        json!({ "sub": "s", "email": "grace@example.com", "proved": true, "name": "  " }),
    );

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(
        api.database().owner_reads("SELECT name FROM users").await,
        ["grace"]
    );
}

#[tokio::test]
async fn auth_075_a_provider_that_proves_the_address_registers_it_proved_and_sends_no_code() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(response.json()["email_verification_required"], false);
    assert_eq!(
        api.database()
            .owner_reads("SELECT email_verified::text FROM users")
            .await,
        ["true"]
    );
    assert!(
        api.database()
            .owner_reads("SELECT kind FROM jobs WHERE kind = 'auth.send_verification_code'")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn auth_075_a_sign_in_whose_provider_proves_the_account_s_address_proves_it() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", false));
    signed_in(&api).await;
    gives(&upstream, identity("subject-1", "ADA@example.com", true));

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(response.json()["email_verification_required"], false);
    assert_eq!(
        api.database()
            .owner_reads("SELECT action FROM audit_entries WHERE action = 'email.verified'")
            .await,
        ["email.verified"]
    );
}

#[tokio::test]
async fn auth_075_a_sign_in_whose_provider_proves_another_address_proves_nothing() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", false));
    signed_in(&api).await;
    gives(
        &upstream,
        identity("subject-1", "ada@elsewhere.example", true),
    );

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.json()["email_verification_required"], true);
}

#[tokio::test]
async fn auth_076_an_address_that_an_account_holds_answers_409_and_writes_nothing() {
    let (upstream, api) = standing().await;
    users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "auth.email_in_use");
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM provider_identities")
            .await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_076_a_provider_that_gives_no_address_answers_422() {
    let (upstream, api) = standing().await;
    gives(&upstream, json!({ "sub": "s", "proved": false }));
    let response = sign_in(&api, &grant()).await;
    assert_eq!(response.status, 422);
    assert_eq!(response.json()["code"], "auth.provider_address_missing");

    gives(
        &upstream,
        json!({ "sub": "s", "email": "no-at-sign", "proved": false }),
    );
    let response = sign_in(&api, &grant()).await;
    assert_eq!(response.status, 422);
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM users")
            .await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_077_invalid_grant_answers_401_and_any_other_refusal_or_failure_503() {
    let (upstream, api) = standing().await;
    upstream.set("POST", "/token", 400, json!({ "error": "invalid_grant" }));
    let response = sign_in(&api, &grant()).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "auth.provider_code_rejected");

    upstream.set("POST", "/token", 401, json!({ "error": "invalid_client" }));
    let response = sign_in(&api, &grant()).await;
    assert_eq!(response.status, 503, "a refusal of the API's own client");

    upstream.set("POST", "/token", 500, json!({}));
    let response = sign_in(&api, &grant()).await;
    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "auth.provider_unavailable");
    assert_eq!(response.header("retry-after"), "30");

    upstream.set("POST", "/token", 200, json!({ "access_token": "t" }));
    upstream.set("GET", "/identity", 200, json!({ "unexpected": true }));
    let response = sign_in(&api, &grant()).await;
    assert_eq!(response.status, 503, "an answer the API can't read");
}

#[tokio::test]
async fn auth_077_a_provider_that_doesn_t_answer_answers_503_at_the_exchange_s_deadline() {
    let (upstream, api) = standing().await;
    upstream.hang("POST", "/token");
    let clock = api.clock();
    let call = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api, &grant()).await })
    };

    clock.sleeping(std::time::Duration::from_secs(6)).await;
    clock.advance(std::time::Duration::from_secs(6));

    let response = call.await.unwrap();
    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "auth.provider_unavailable");
}

#[tokio::test]
async fn auth_077_an_unknown_provider_answers_404_and_an_unconfigured_one_503() {
    let api = Api::new().await;

    let mut nobody = grant();
    nobody["provider"] = json!("nobody");
    let unknown = api.post("/v1/sessions/provider", &nobody).send().await;
    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.json()["code"], "auth.provider_not_found");

    let unconfigured = sign_in(&api, &grant()).await;
    assert_eq!(unconfigured.status, 503);
    assert_eq!(unconfigured.json()["code"], "request.feature_disabled");
}

#[tokio::test]
async fn auth_077_a_malformed_code_verifier_or_redirect_uri_answers_400_before_the_exchange() {
    let (upstream, api) = standing().await;
    let malformed = json!({
        "provider": "stand_in",
        "code": "a code with spaces",
        "code_verifier": "too-short",
        "redirect_uri": "https://evil.example/callback",
    });
    let response = api
        .post_raw("/v1/sessions/provider", &malformed)
        .send()
        .await;
    assert_eq!(response.status, 400);
    let body = response.json();
    let fields: Vec<&str> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| error["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        ["/code", "/code_verifier", "/redirect_uri"],
        "API-10"
    );

    for code in [String::new(), "c".repeat(2_049)] {
        let mut body = grant();
        body["code"] = json!(code);
        let response = api.post_raw("/v1/sessions/provider", &body).send().await;
        assert_eq!(response.status, 400);
        assert_eq!(response.json()["errors"][0]["field"], "/code");
    }

    let unlisted = json!({
        "provider": "stand_in",
        "code": "a-code",
        "code_verifier": VERIFIER,
        "redirect_uri": "https://evil.example/callback",
    });
    let response = sign_in(&api, &unlisted).await;
    assert_eq!(response.status, 400);
    assert_eq!(response.json()["errors"][0]["field"], "/redirect_uri");
    assert_eq!(response.json()["errors"][0]["code"], "invalid_redirect_uri");
    assert!(upstream.received().is_empty());
}

#[tokio::test]
async fn auth_078_a_loopback_redirect_uri_listed_without_a_port_matches_any_port() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let mut body = grant();
    body["redirect_uri"] = json!("http://127.0.0.1:51234/callback");

    let response = sign_in(&api, &body).await;

    assert_eq!(response.status, 201, "{}", response.json());
    let exchange = &upstream.received()[0];
    assert_eq!(
        (exchange.method.as_str(), exchange.path.as_str()),
        ("POST", "/token")
    );
    assert_eq!(
        exchange.field("redirect_uri").as_deref(),
        Some("http://127.0.0.1:51234/callback")
    );
    assert_eq!(
        exchange.field("client_id").as_deref(),
        Some("stand-in-client")
    );
    assert_eq!(exchange.field("code_verifier").as_deref(), Some(VERIFIER));
}

#[tokio::test]
async fn auth_079_the_provider_s_token_reaches_no_row_and_no_answer() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", false));

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201);
    assert!(!String::from_utf8_lossy(&response.body).contains("a-provider-token"));
    assert_eq!(
        upstream.received()[1].authorization.as_deref(),
        Some("Bearer a-provider-token")
    );
    let found = api
        .database()
        .owner_reads(
            "SELECT count(*)::text FROM (\
               SELECT details::text AS value FROM audit_entries \
               UNION ALL SELECT payload::text FROM jobs \
               UNION ALL SELECT payload::text FROM events) AS stored \
             WHERE value LIKE '%a-provider-token%'",
        )
        .await;
    assert_eq!(found, ["0"]);
}

/// Re-authenticates `user` through the stand-in, with `extra` in the body.
async fn reauthenticate(api: &Api, user: &SignedIn, extra: Value) -> Response {
    let mut body = grant();
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    api.post(
        "/v1/users/me/sessions/current/reauthenticate/provider",
        &body,
    )
    .bearer(&user.access)
    .send()
    .await
}

#[tokio::test]
async fn auth_080_the_caller_s_subject_reauthenticates_the_session() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    users::authenticated_ago(&api, &ada, 10).await;

    let response = reauthenticate(&api, &ada, json!({})).await;

    assert_eq!(response.status, 200, "{}", response.json());
    let deleted = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/others")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(deleted.status, 204, "the proof holds (AUTH-045)");
    assert!(
        provider_entries(&api)
            .await
            .contains(&"session.reauthenticated".to_owned())
    );
}

#[tokio::test]
async fn auth_080_another_subject_answers_403_records_it_and_changes_nothing() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    users::authenticated_ago(&api, &ada, 10).await;
    gives(&upstream, identity("subject-2", "ada@example.com", true));

    let response = reauthenticate(&api, &ada, json!({})).await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "auth.wrong_identity");
    assert_eq!(
        api.database()
            .owner_reads("SELECT action FROM audit_entries WHERE action = 'identity.check_failed'")
            .await,
        ["identity.check_failed"]
    );
    let deleted = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/others")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(deleted.status, 403, "no proof");
}

#[tokio::test]
async fn auth_080_an_account_with_an_app_needs_its_code_beside_the_provider_s() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    let app = second_factors::add_app(&api, &ada).await;

    let missing = reauthenticate(&api, &ada, json!({})).await;
    assert_eq!(missing.status, 403);
    assert_eq!(missing.json()["code"], "auth.second_factor_required");

    let code = second_factors::code(&api, &app.secret).await;
    let response = reauthenticate(&api, &ada, json!({ "second_factor_code": code })).await;
    assert_eq!(response.status, 200, "{}", response.json());
}

#[tokio::test]
async fn auth_081_startup_refuses_a_client_with_part_of_its_settings_or_a_wrong_redirect_uri() {
    let partial = Options::default().set("STAND_IN_CLIENT_ID", "client");
    assert_eq!(api::refusal(&partial), "STAND_IN_CLIENT_SECRET is required");
    let configured = |uris: &str| {
        Options::default()
            .set("STAND_IN_CLIENT_ID", "client")
            .set("STAND_IN_CLIENT_SECRET", "secret")
            .set("STAND_IN_REDIRECT_URIS", uris)
    };
    for wrong in [
        "http://app.example.com/callback",
        "https://app.example.com/callback#fragment",
        "app://callback",
        "http://127.0.0.2/callback",
    ] {
        let refusal = api::refusal(&configured(wrong));
        assert!(
            refusal.starts_with("STAND_IN_REDIRECT_URIS must be"),
            "{wrong}: {refusal}"
        );
    }
}

#[tokio::test]
async fn auth_081_startup_refuses_a_provider_s_setting_without_a_client_and_a_client_without_it() {
    let alone = Options::default().set("STAND_IN_NAME", "Stand-in");
    assert_eq!(api::refusal(&alone), "STAND_IN_CLIENT_ID is required");

    let client = Options::default()
        .set("STAND_IN_CLIENT_ID", "client")
        .set("STAND_IN_CLIENT_SECRET", "secret")
        .set("STAND_IN_REDIRECT_URIS", "https://app.example.com/callback");
    assert_eq!(api::refusal(&client), "STAND_IN_NAME is required");

    // With both, the provider is on, and with neither, it is off.
    api::configuration(&client.set("STAND_IN_NAME", "Stand-in"));
    api::configuration(&Options::default());
}

#[tokio::test]
async fn auth_081_startup_refuses_a_redirect_uri_that_two_clients_list_or_match() {
    let clients = |first: &str, second: &str| {
        Options::default()
            .set("STAND_IN_NAME", "Stand-in")
            .set("STAND_IN_CLIENT_ID", "first")
            .set("STAND_IN_CLIENT_SECRET", "secret")
            .set("STAND_IN_REDIRECT_URIS", first)
            .set("STAND_IN_SECOND_CLIENT_ID", "second")
            .set("STAND_IN_SECOND_CLIENT_SECRET", "secret")
            .set("STAND_IN_SECOND_REDIRECT_URIS", second)
    };
    for (first, second) in [
        (
            "https://app.example.com/callback",
            "https://app.example.com/callback",
        ),
        (
            "http://127.0.0.1/callback",
            "http://127.0.0.1:5000/callback",
        ),
    ] {
        let refusal = api::refusal(&clients(first, second));
        assert!(
            refusal.starts_with("STAND_IN_SECOND_REDIRECT_URIS must be"),
            "{first} and {second}: {refusal}"
        );
    }
    api::configuration(&clients(
        "http://127.0.0.1:4000/callback",
        "http://127.0.0.1:5000/callback",
    ));
}

#[tokio::test]
async fn auth_081_the_capabilities_list_each_provider_that_the_deployment_configures() {
    let (_upstream, api) = standing().await;

    let capabilities = api.get("/v1/capabilities").send().await.json();

    assert_eq!(
        capabilities["sign_in_providers"],
        json!([{
            "provider": "stand_in",
            "name": "Stand-in",
            "authorization_endpoint": "https://provider.test/authorize",
            "scopes": ["identity"],
            "clients": [{
                "client_id": "stand-in-client",
                "redirect_uris": ["https://app.example.com/callback", "http://127.0.0.1/callback"],
            }],
        }])
    );
    let unconfigured = Api::new().await;
    let capabilities = unconfigured.get("/v1/capabilities").send().await.json();
    assert_eq!(capabilities["sign_in_providers"], json!([]));
}

#[tokio::test]
async fn sec_10_the_configured_client_s_test_origin_is_refused_outside_the_test_environment() {
    let options = Options::default()
        .set("ENVIRONMENT", "development")
        .set("OUTBOUND_TEST_ORIGIN", "http://127.0.0.1:4000");
    assert_eq!(
        api::refusal(&options),
        "OUTBOUND_TEST_ORIGIN must be unset outside the test environment (SEC-10)"
    );
    let not_loopback = Options::default().set("OUTBOUND_TEST_ORIGIN", "http://example.com:4000");
    assert!(api::refusal(&not_loopback).starts_with("OUTBOUND_TEST_ORIGIN must be an http origin"));
}

#[tokio::test]
async fn auth_025_an_account_that_must_verify_re_authenticates_through_its_provider() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", false));
    let ada = signed_in(&api).await;
    let refused = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(refused.json()["code"], "request.email_unverified");

    let response = reauthenticate(&api, &ada, json!({})).await;

    assert_eq!(response.status, 200, "{}", response.json());
    assert_eq!(response.json()["email_verification_required"], true);
}

#[tokio::test]
async fn auth_029_a_registration_through_a_provider_that_sends_a_code_counts_in_its_bucket() {
    let (upstream, api) = standing().await;
    // A registration with a password counts once in the address's bucket of 3.
    users::register_unverified(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", false));

    for _ in 0..2 {
        assert_eq!(sign_in(&api, &grant()).await.status, 409);
    }
    let limited = sign_in(&api, &grant()).await;

    assert_eq!(limited.status, 429);
    assert_eq!(limited.json()["code"], "request.rate_limited");
}

#[tokio::test]
async fn auth_029_a_linked_account_s_sign_ins_count_in_no_registration_bucket() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", false));

    for _ in 0..5 {
        assert_eq!(sign_in(&api, &grant()).await.status, 201);
    }
}

#[tokio::test]
async fn auth_046_a_provider_s_registration_and_sign_in_publish_session_created() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let first = signed_in(&api).await;
    let second = signed_in(&api).await;

    let mut expected = vec![
        format!("{} {}", first.user_id, first.session_id),
        format!("{} {}", second.user_id, second.session_id),
    ];
    expected.sort();
    assert_eq!(
        users::session_events(&api, "session.created").await,
        expected
    );
}

#[tokio::test]
async fn auth_080_a_code_the_provider_refuses_answers_403_since_the_session_still_signs_in() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    upstream.set("POST", "/token", 400, json!({ "error": "invalid_grant" }));

    let response = reauthenticate(&api, &ada, json!({})).await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "auth.provider_proof_rejected");
}

#[tokio::test]
async fn auth_080_an_account_without_an_identity_of_the_provider_answers_403() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));

    let response = reauthenticate(&api, &ada, json!({})).await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "auth.wrong_identity");
}

#[tokio::test]
async fn auth_080_re_authenticating_counts_in_the_bucket_of_the_user_s_checks() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    gives(&upstream, identity("subject-2", "ada@example.com", true));

    for _ in 0..5 {
        assert_eq!(reauthenticate(&api, &ada, json!({})).await.status, 403);
    }
    let limited = reauthenticate(&api, &ada, json!({})).await;

    assert_eq!(limited.status, 429, "AUTH-022's 5 checks in 15 minutes");
}

#[tokio::test]
async fn auth_080_a_session_that_another_request_ended_meanwhile_answers_401() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    let db = api.database();
    api.get("/v1/users/me").bearer(&ada.access).send().await;
    let checks = users::added_bucket(&api, reauthenticate(&api, &ada, json!({}))).await;
    // The re-authentication waits at its bucket while another request ends its session.
    let mut ender = db
        .holding(&format!(
            "SELECT 1 FROM rate_limit_buckets WHERE id = '{checks}' FOR UPDATE"
        ))
        .await;

    let again = tokio::spawn({
        let api = api.clone();
        let ada = SignedIn {
            user_id: ada.user_id.clone(),
            session_id: ada.session_id.clone(),
            access: ada.access.clone(),
            refresh: ada.refresh.clone(),
        };
        async move {
            let response = reauthenticate(&api, &ada, json!({})).await;
            (response.status, response.json())
        }
    });
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut ender,
        &format!("DELETE FROM sessions WHERE id = '{}'", ada.session_id),
    )
    .await;
    execute_on(&mut ender, "COMMIT").await;
    let (status, body) = again.await.unwrap();

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
}

#[tokio::test]
async fn accounts_013_a_deletion_erases_the_user_s_provider_identities() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;

    let deleted = api
        .request(http::Method::DELETE, "/v1/users/me")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(deleted.status, 204, "{}", deleted.json());
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM provider_identities")
            .await,
        ["0"]
    );
}

/// Whether a line that the tests' telemetry holds contains `text`.
fn logged(text: &str) -> bool {
    testing::all_logged()
        .iter()
        .any(|line| line.to_string().contains(text))
}

#[tokio::test]
async fn auth_077_the_log_of_a_refusal_names_its_status_and_rfc_6749_s_code_alone() {
    let (upstream, api) = standing().await;
    upstream.set(
        "POST",
        "/token",
        401,
        json!({ "error": "invalid_client", "error_description": "detail-that-no-log-holds" }),
    );
    assert_eq!(sign_in(&api, &grant()).await.status, 503);
    upstream.set(
        "POST",
        "/token",
        400,
        json!({ "error": "a_code_that_no_log_holds" }),
    );
    assert_eq!(sign_in(&api, &grant()).await.status, 503);

    let failures: Vec<String> = testing::all_logged()
        .into_iter()
        .filter(|line| line["error.type"] == "auth.provider_unavailable")
        .map(|line| {
            line["exception.message"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert!(
        failures
            .iter()
            .any(|cause| cause.contains("stand_in")
                && cause.contains("invalid_client, and status 401")),
        "{failures:?}"
    );
    assert!(
        failures
            .iter()
            .any(|cause| cause.contains("another code, and status 400")),
        "{failures:?}"
    );
    assert!(!logged("detail-that-no-log-holds"));
    assert!(!logged("a_code_that_no_log_holds"));
}

#[tokio::test]
async fn auth_079_the_provider_s_token_reaches_no_log() {
    let (upstream, api) = standing().await;
    upstream.set(
        "POST",
        "/token",
        200,
        json!({ "access_token": "a-token-that-no-log-holds" }),
    );
    upstream.set(
        "GET",
        "/identity",
        200,
        identity("subject-1", "ada@example.com", true),
    );

    assert_eq!(sign_in(&api, &grant()).await.status, 201);

    assert!(!logged("a-token-that-no-log-holds"));
}

#[tokio::test]
async fn auth_080_a_wrong_second_factor_answers_403_and_a_recovery_code_proves_it() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    let app = second_factors::add_app(&api, &ada).await;
    let right = second_factors::code(&api, &app.secret).await;

    let wrong = reauthenticate(
        &api,
        &ada,
        json!({ "second_factor_code": second_factors::other_code(&right) }),
    )
    .await;
    assert_eq!(wrong.status, 403);
    assert_eq!(wrong.json()["code"], "auth.wrong_second_factor");
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT action FROM audit_entries WHERE action = 'second_factor.check_failed'"
            )
            .await,
        ["second_factor.check_failed"]
    );

    let recovered = reauthenticate(
        &api,
        &ada,
        json!({ "recovery_code": app.recovery_codes[0] }),
    )
    .await;
    assert_eq!(recovered.status, 200, "{}", recovered.json());
}

#[tokio::test]
async fn auth_073_a_device_token_of_the_account_s_email_ends_and_one_of_another_counts_as_none() {
    let (upstream, api) = standing().await;
    let bob = api
        .post("/v1/users", &users::registration("bob@example.com"))
        .send()
        .await;
    let bobs_token = bob.json()["device_token"].as_str().unwrap().to_owned();
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let first = sign_in(&api, &grant()).await;
    let adas_token = first.json()["device_token"].as_str().unwrap().to_owned();
    let user = first.json()["user_id"].as_str().unwrap().to_owned();
    let tokens = |user: String| {
        let api = api.clone();
        async move {
            api.database()
                .owner_reads(&format!(
                    "SELECT count(*)::text FROM device_tokens WHERE user_id = '{user}'"
                ))
                .await
                .remove(0)
        }
    };

    let mut body = grant();
    body["device_token"] = json!(adas_token);
    let again = sign_in(&api, &body).await;
    assert_eq!(again.status, 201);
    assert_ne!(again.json()["device_token"], json!(adas_token));
    assert_eq!(tokens(user.clone()).await, "1", "the one it brought ended");

    body["device_token"] = json!(bobs_token);
    assert_eq!(sign_in(&api, &body).await.status, 201);
    assert_eq!(
        tokens(user).await,
        "2",
        "another email's counts as none, and ends nothing"
    );
    let bob_id = bob.json()["user_id"].as_str().unwrap().to_owned();
    assert_eq!(tokens(bob_id).await, "1");
}

#[tokio::test]
async fn auth_008_a_sign_in_through_a_provider_on_a_device_replaces_its_session() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let mut body = grant();
    body["device_id"] = json!("laptop");

    let first = sign_in(&api, &body).await;
    let second = sign_in(&api, &body).await;

    assert_eq!(second.status, 201);
    assert_eq!(
        api.database()
            .owner_reads("SELECT id::text FROM sessions WHERE device_id = 'laptop'")
            .await,
        [second.json()["id"].as_str().unwrap()]
    );
    assert_ne!(first.json()["id"], second.json()["id"]);
}

#[tokio::test]
async fn auth_015_a_browser_s_sign_in_through_a_provider_takes_its_tokens_in_cookies() {
    let upstream = Upstream::start().await;
    let api =
        Api::with(providers::stand_in(&upstream).set("CORS_ALLOWED_ORIGINS", users::WEB)).await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));

    let response = api
        .post("/v1/sessions/provider", &grant())
        .header("origin", users::WEB)
        .send()
        .await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert!(response.json().get("access_token").is_none());
    let cookies = users::cookies(&response);
    assert!(cookies.contains("__Host-access_token="), "{cookies}");
    assert!(cookies.contains("__Host-refresh_token="), "{cookies}");
    assert!(cookies.contains("__Host-device_token_"), "{cookies}");
}
