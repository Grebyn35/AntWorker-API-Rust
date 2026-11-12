//! Signing in, registering and re-authenticating through a provider, which every provider shares,
//! proven through the tests' stand-in provider (the auth spec's "Signing in through a provider").

use serde_json::{Value, json};

use crate::support::{
    Api,
    api::{self, Options, Response},
    second_factors,
    upstream::Upstream,
    users::{self, SignedIn},
};

/// A verifier of a PKCE challenge, of the 43 characters that RFC 7636 asks at least.
const VERIFIER: &str = "a-verifier-of-at-least-43-characters.0123456789";

/// The stand-in's browser redirect URI, and its loopback one.
const REDIRECT_URIS: &str = "https://app.example.com/callback,http://127.0.0.1/callback";

/// The options of an API whose stand-in provider has one client, which answers at `upstream`.
fn configured(upstream: &Upstream) -> Options {
    upstream
        .options()
        .set("STAND_IN_CLIENT_ID", "stand-in-client")
        .set("STAND_IN_CLIENT_SECRET", "stand-in-secret")
        .set("STAND_IN_REDIRECT_URIS", REDIRECT_URIS)
}

/// A stand-in provider, and an API that signs in through it.
async fn standing() -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let api = Api::with(configured(&upstream)).await;
    (upstream, api)
}

/// Makes the stand-in give a token for any code, and `identity` for it.
fn gives(upstream: &Upstream, identity: Value) {
    upstream.set(
        "POST",
        "/token",
        200,
        json!({ "access_token": "a-provider-token" }),
    );
    upstream.set("GET", "/identity", 200, identity);
}

/// The identity of `subject`, with `email`, which the provider proves when `proved` says so.
fn identity(subject: &str, email: &str, proved: bool) -> Value {
    json!({ "subject": subject, "email": email, "proved": proved, "name": "Ada Lovelace" })
}

/// The body of a sign-in through the stand-in, with its browser redirect URI.
fn grant() -> Value {
    json!({
        "provider": "stand_in",
        "code": "a-code",
        "code_verifier": VERIFIER,
        "redirect_uri": "https://app.example.com/callback",
    })
}

async fn sign_in(api: &Api, body: &Value) -> Response {
    api.post("/v1/sessions/provider", body).send().await
}

/// Signs in through the stand-in, which must start a session.
async fn signed_in(api: &Api) -> SignedIn {
    let response = sign_in(api, &grant()).await;
    assert_eq!(response.status, 201, "{}", response.json());
    SignedIn::from(&response)
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
        json!({ "subject": "s", "email": "grace@example.com", "proved": true, "name": "  " }),
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
    gives(&upstream, json!({ "subject": "s", "proved": false }));
    let response = sign_in(&api, &grant()).await;
    assert_eq!(response.status, 422);
    assert_eq!(response.json()["code"], "auth.provider_address_missing");

    gives(
        &upstream,
        json!({ "subject": "s", "email": "no-at-sign", "proved": false }),
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
        "redirect_uri": "https://app.example.com/callback",
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
    assert_eq!(fields, ["/code", "/code_verifier"]);

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
        "https://app.example.com/callback,https://app.example.com/callback",
    ] {
        let refusal = api::refusal(&configured(wrong));
        assert!(
            refusal.starts_with("STAND_IN_REDIRECT_URIS must be"),
            "{wrong}: {refusal}"
        );
    }
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
