//! Signing in, registering and re-authenticating through a provider, which every provider shares,
//! proven through the tests' stand-in provider (the auth spec's "Signing in through a provider").

use platform::telemetry::testing;
use serde_json::{Value, json};

use crate::support::{
    Api, TestDatabase,
    api::{self, Options, Response},
    execute_on,
    providers::{
        self, VERIFIER, gives, grant, identity, kept_token_logged, kept_tokens_counted, sign_in,
        signed_in,
    },
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

/// The redirect URI of the stand-in's second client, whose tokens don't lapse.
const SECOND_REDIRECT_URI: &str = "https://second.example.com/callback";

/// A stand-in provider with both of its clients, and an API that signs in through it.
async fn standing_with_both_clients() -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let options = providers::stand_in(&upstream)
        .set("STAND_IN_SECOND_CLIENT_ID", "second-client")
        .set("STAND_IN_SECOND_CLIENT_SECRET", "second-secret")
        .set("STAND_IN_SECOND_REDIRECT_URIS", SECOND_REDIRECT_URI);
    let api = Api::with(options).await;
    (upstream, api)
}

/// The body of a sign-in through the stand-in's second client.
fn second_grant() -> Value {
    let mut body = grant();
    body["redirect_uri"] = json!(SECOND_REDIRECT_URI);
    body
}

/// The deletions of a token that the stand-in received, each as its `Authorization`.
fn deletions(upstream: &Upstream) -> Vec<Option<String>> {
    let received = upstream.received();
    let deletions = received
        .iter()
        .filter(|request| request.method == "DELETE" && request.path == "/token");
    deletions
        .map(|request| request.authorization.clone())
        .collect()
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

/// The scope, the log, the actor, the resource and the details of each entry of `action`, the
/// actor `-` when there is none, in order (SEC-18).
async fn entries(api: &Api, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT scope || ' ' || user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' \
             || resource_type || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = '{action}' ORDER BY id"
        ))
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
async fn auth_074_a_part_before_the_at_of_more_than_100_characters_is_cut_to_100() {
    let (upstream, api) = standing().await;
    let local = format!("{}{}", "a".repeat(100), "b".repeat(20));
    gives(
        &upstream,
        json!({ "sub": "s", "email": format!("{local}@example.com"), "proved": true }),
    );

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(
        api.database().owner_reads("SELECT name FROM users").await,
        ["a".repeat(100)]
    );
}

#[tokio::test]
async fn auth_074_a_registration_through_a_provider_records_user_registered_with_the_provider() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    let user = response.json()["user_id"].as_str().unwrap().to_owned();
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT action || ' ' || scope || ' ' || user_id || ' ' || actor_user_id || ' ' \
                 || resource_type || ' ' || resource_id || ' ' || details::text \
                 FROM audit_entries WHERE action = 'user.registered'"
            )
            .await,
        [format!(
            "user.registered user {user} {user} user {user} {{\"provider\": \"stand_in\"}}"
        )]
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
async fn auth_075_with_email_off_a_provider_that_proves_the_address_registers_it_proved() {
    let upstream = Upstream::start().await;
    let api = Api::with(providers::stand_in(&upstream).set("EMAIL", "off")).await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
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
    let ada = response.json()["user_id"].as_str().unwrap().to_owned();
    assert_eq!(
        entries(&api, "email.verified").await,
        [format!("user {ada} - user {ada} {{}}")]
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
async fn auth_077_the_exchange_sends_the_client_the_code_its_verifier_and_its_redirect_uri() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));

    signed_in(&api).await;

    let exchange = &upstream.received()[0];
    assert_eq!(exchange.method, "POST");
    assert_eq!(exchange.path, "/token");
    assert_eq!(
        exchange.authorization, None,
        "the client's ID and secret go in the request's body"
    );
    for (field, value) in [
        ("grant_type", "authorization_code"),
        ("client_id", "stand-in-client"),
        ("client_secret", "stand-in-secret"),
        ("code", "a-code"),
        ("code_verifier", VERIFIER),
        ("redirect_uri", "https://app.example.com/callback"),
    ] {
        assert_eq!(exchange.field(field).as_deref(), Some(value), "{field}");
    }
}

/// Asserts that `response` is AUTH-077's answer of a provider that the API can't exchange at.
fn unavailable(response: &Response) {
    assert_eq!(response.status, 503, "{}", response.json());
    assert_eq!(response.json()["code"], "auth.provider_unavailable");
    assert_eq!(response.header("retry-after"), "30");
}

/// How many accounts there are.
async fn accounts(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads("SELECT count(*)::text FROM users")
        .await
}

#[tokio::test]
async fn auth_077_invalid_grant_answers_401() {
    let (upstream, api) = standing().await;
    upstream.set("POST", "/token", 400, json!({ "error": "invalid_grant" }));

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "auth.provider_code_rejected");
}

#[tokio::test]
async fn auth_077_a_refusal_of_the_api_s_own_client_answers_503() {
    let (upstream, api) = standing().await;
    upstream.set("POST", "/token", 401, json!({ "error": "invalid_client" }));

    let response = sign_in(&api, &grant()).await;

    unavailable(&response);
}

#[tokio::test]
async fn auth_077_a_server_error_answers_503() {
    let (upstream, api) = standing().await;
    upstream.set("POST", "/token", 500, json!({}));

    let response = sign_in(&api, &grant()).await;

    unavailable(&response);
}

#[tokio::test]
async fn auth_077_an_identity_that_the_api_can_t_read_answers_503() {
    let (upstream, api) = standing().await;
    upstream.set("POST", "/token", 200, json!({ "access_token": "t" }));
    upstream.set("GET", "/identity", 200, json!({ "unexpected": true }));

    let response = sign_in(&api, &grant()).await;

    unavailable(&response);
    assert_eq!(accounts(&api).await, ["0"]);
}

#[tokio::test]
async fn auth_077_an_identity_without_a_subject_answers_503() {
    let (upstream, api) = standing().await;
    upstream.set("POST", "/token", 200, json!({ "access_token": "t" }));
    let nameless = json!({ "sub": "", "email": "ada@example.com" });
    upstream.set("GET", "/identity", 200, nameless);

    let response = sign_in(&api, &grant()).await;

    unavailable(&response);
    assert_eq!(accounts(&api).await, ["0"]);
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
async fn auth_077_a_provider_whose_identity_doesn_t_come_answers_503_at_the_exchange_s_deadline() {
    let (upstream, api) = standing().await;
    upstream.set("POST", "/token", 200, json!({ "access_token": "t" }));
    upstream.hang("GET", "/identity");
    let clock = api.clock();
    let call = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api, &grant()).await })
    };

    // The token came, and the read of the identity waits.
    while !upstream
        .received()
        .iter()
        .any(|received| received.method == "GET" && received.path == "/identity")
    {
        tokio::task::yield_now().await;
    }
    clock.sleeping(std::time::Duration::from_secs(6)).await;
    clock.advance(std::time::Duration::from_secs(6));

    let response = call.await.unwrap();
    unavailable(&response);
}

#[tokio::test]
async fn auth_100_sign_ins_that_wait_for_the_provider_hold_no_connection_of_the_pool() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    upstream.hang("POST", "/token");
    // More sign-ins than the pool has connections, each of which waits for the provider.
    let waiting: Vec<_> = (0..usize::try_from(TestDatabase::POOL_SIZE).unwrap() + 2)
        .map(|_| {
            let api = api.clone();
            tokio::spawn(async move { sign_in(&api, &grant()).await })
        })
        .collect();
    while exchanges(&upstream).len() < waiting.len() {
        tokio::task::yield_now().await;
    }

    // Other requests, each of which needs a connection of the pool, are served meanwhile.
    let capabilities = api.get("/v1/capabilities").send().await;
    let record = api.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(capabilities.status, 200);
    assert_eq!(record.status, 200);
    let clock = api.clock();
    clock.sleeping(std::time::Duration::from_secs(6)).await;
    clock.advance(std::time::Duration::from_secs(6));
    for sign_in in waiting {
        unavailable(&sign_in.await.unwrap());
    }
}

#[tokio::test]
async fn auth_100_an_unknown_provider_answers_404_and_an_unconfigured_one_503() {
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
async fn auth_100_a_malformed_code_verifier_or_redirect_uri_answers_400_before_the_exchange() {
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
    assert_eq!(
        refused_fields(&response),
        [
            "/code invalid_code",
            "/code_verifier invalid_code_verifier",
            "/redirect_uri invalid_redirect_uri"
        ],
        "API-10"
    );

    for code in [String::new(), "c".repeat(2_049)] {
        let mut body = grant();
        body["code"] = json!(code);
        let response = api.post_raw("/v1/sessions/provider", &body).send().await;
        assert_eq!(response.status, 400);
        assert_eq!(refused_fields(&response), ["/code invalid_code"]);
    }
    // 42 characters, 129, and 43 with one that RFC 7636 doesn't allow.
    let short = "v".repeat(42);
    for verifier in [short.clone(), "v".repeat(129), format!("{short}+")] {
        let mut body = grant();
        body["code_verifier"] = json!(verifier);
        let response = api.post_raw("/v1/sessions/provider", &body).send().await;
        assert_eq!(response.status, 400, "{verifier}");
        assert_eq!(
            refused_fields(&response),
            ["/code_verifier invalid_code_verifier"],
            "{verifier}"
        );
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
async fn auth_100_a_code_of_2048_characters_and_verifiers_of_43_and_128_reach_the_exchange() {
    let (upstream, api) = standing().await;
    upstream.set("POST", "/token", 400, json!({ "error": "invalid_grant" }));

    for (code, verifier) in [
        ("c".repeat(2_048), "v".repeat(43)),
        ("a-code".to_owned(), format!("{}-._~", "V9".repeat(62))),
    ] {
        let mut body = grant();
        body["code"] = json!(code);
        body["code_verifier"] = json!(verifier);
        let response = sign_in(&api, &body).await;
        assert_eq!(response.status, 401, "{}", response.json());
    }

    assert_eq!(exchanges(&upstream).len(), 2);
}

/// Each field that `response`, a 400, names, with its code.
fn refused_fields(response: &Response) -> Vec<String> {
    response.json()["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| format!("{} {}", error["field"], error["code"]).replace('"', ""))
        .collect()
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
    // With the tokens that the proof gave the session (AUTH-095).
    let deleted = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/others")
        .bearer(&SignedIn::from(&response).access)
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
async fn auth_101_another_subject_answers_403_records_it_and_changes_nothing() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    users::authenticated_ago(&api, &ada, 10).await;
    gives(&upstream, identity("subject-2", "ada@example.com", true));

    let response = reauthenticate(&api, &ada, json!({})).await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "auth.wrong_identity");
    let id = &ada.user_id;
    assert_eq!(
        entries(&api, "identity.check_failed").await,
        [format!(
            "user {id} {id} user {id} {{\"provider\": \"stand_in\"}}"
        )]
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
async fn auth_099_a_linked_account_s_sign_ins_count_in_no_registration_bucket() {
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
async fn auth_101_an_account_without_an_identity_of_the_provider_answers_403() {
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
async fn auth_101_a_session_that_another_request_ended_meanwhile_answers_401() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    let db = api.database();
    api.get("/v1/users/me").bearer(&ada.access).send().await;
    let (checks, first) = users::added_bucket_of(&api, reauthenticate(&api, &ada, json!({}))).await;
    // The proof gave the session new tokens (AUTH-095).
    let ada = SignedIn::from(&first);
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
async fn auth_100_the_log_of_a_refusal_names_its_status_and_rfc_6749_s_code_alone() {
    let (upstream, api) = standing().await;
    upstream.set(
        "POST",
        "/token",
        401,
        json!({ "error": "invalid_client", "error_description": "detail-that-no-log-holds" }),
    );
    let client_refused = sign_in(&api, &grant()).await;
    upstream.set(
        "POST",
        "/token",
        400,
        json!({ "error": "a_code_that_no_log_holds" }),
    );
    let other_code = sign_in(&api, &grant()).await;

    // The answer holds nothing of what the provider sent, its code of a refusal included.
    for response in [&client_refused, &other_code] {
        let mut body = response.json();
        body["trace_id"] = json!(null);
        assert_eq!(
            body,
            json!({
                "type": "/problems/auth.provider_unavailable",
                "title": "Provider unavailable",
                "status": 503,
                "code": "auth.provider_unavailable",
                "trace_id": null,
            })
        );
    }

    // The lines of these two requests, by their traces: other tests' refusals log the same.
    let failures: Vec<String> = [&client_refused, &other_code]
        .into_iter()
        .flat_map(|response| testing::logged(response.trace_id()))
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
async fn auth_079_a_token_that_doesn_t_lapse_is_deleted_whether_or_not_the_identity_was_read() {
    let (upstream, api) = standing_with_both_clients().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    upstream.set("DELETE", "/token", 204, json!({}));

    // The first client's tokens lapse, and the second's don't.
    let lapsing = sign_in(&api, &grant()).await;
    let after_lapsing = deletions(&upstream);
    let read = sign_in(&api, &second_grant()).await;
    let after_read = deletions(&upstream);
    upstream.set("GET", "/identity", 500, json!({}));
    let unread = sign_in(&api, &second_grant()).await;

    assert_eq!(lapsing.status, 201, "{}", lapsing.json());
    assert!(after_lapsing.is_empty(), "{after_lapsing:?}");
    assert_eq!(read.status, 201, "{}", read.json());
    let bearer = Some("Bearer a-provider-token".to_owned());
    assert_eq!(after_read, std::slice::from_ref(&bearer));
    assert_eq!(unread.status, 503);
    assert_eq!(deletions(&upstream), [bearer.clone(), bearer]);
}

#[tokio::test]
async fn auth_079_a_deletion_that_fails_is_logged_and_counted_and_refuses_nothing() {
    let (upstream, api) = standing_with_both_clients().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    upstream.set("DELETE", "/token", 500, json!({}));
    let before = kept_tokens_counted();

    let response = sign_in(&api, &second_grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert!(
        kept_token_logged(&response, "answered 500"),
        "ERR-11's line"
    );
    // Tests running alongside ignore errors too, so the count grows by at least this one.
    assert!(kept_tokens_counted() >= before + 1.0, "ERR-11's count");
}

#[tokio::test]
async fn auth_079_a_deletion_that_doesn_t_end_within_2_seconds_refuses_nothing() {
    let (upstream, api) = standing_with_both_clients().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    upstream.hang("DELETE", "/token");
    let clock = api.clock();
    let call = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api, &second_grant()).await })
    };

    // The deletion's deadline waits once its request is sent, so the clock moves only then.
    while deletions(&upstream).is_empty() {
        tokio::task::yield_now().await;
    }
    clock.sleeping(std::time::Duration::from_secs(2)).await;
    clock.advance(std::time::Duration::from_secs(2));

    let response = call.await.unwrap();
    assert_eq!(response.status, 201, "{}", response.json());
    assert!(
        kept_token_logged(&response, "no answer within 2 seconds"),
        "ERR-11's line"
    );
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
    let id = &ada.user_id;
    assert_eq!(
        entries(&api, "second_factor.check_failed").await,
        [format!("user {id} {id} user {id} {{}}")]
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
async fn auth_017_a_sign_in_through_a_provider_takes_a_device_id_of_1_to_64_letters_digits_underscores_and_hyphens()
 {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let (longest, too_long) = ("x".repeat(64), "x".repeat(65));
    for device_id in ["a", "Laptop_2-b", longest.as_str()] {
        let mut body = grant();
        body["device_id"] = json!(device_id);

        let response = sign_in(&api, &body).await;

        assert_eq!(response.status, 201, "{device_id}: {}", response.json());
        assert_eq!(response.json()["device_id"], device_id);
    }
    for device_id in ["", "bad id", "caf\u{e9}", "laptop.1", too_long.as_str()] {
        let mut body = grant();
        body["device_id"] = json!(device_id);

        let response = api.post_raw("/v1/sessions/provider", &body).send().await;

        assert_eq!(response.status, 400, "{device_id}");
        assert_eq!(
            response.json()["errors"],
            json!([{
                "field": "/device_id",
                "code": "invalid_device_id",
                "message": "A device ID has 1 to 64 letters, digits, underscores and hyphens.",
            }])
        );
    }
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

#[tokio::test]
async fn auth_042_a_sign_in_through_a_provider_keeps_its_device_s_name_normalized() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let mut body = grant();
    body["device_name"] = json!("  Ada's \t laptop \n");
    let registered = sign_in(&api, &body).await;
    let mut body = grant();
    body["device_id"] = json!("phone_1");
    body["device_name"] = json!("Ada's phone");
    let phone = sign_in(&api, &body).await;
    let phone_session = api
        .get("/v1/users/me/sessions/current")
        .bearer(&SignedIn::from(&phone).access)
        .send()
        .await;
    // A sign-in that replaces its device's session gives the new one its own name, or none.
    body.as_object_mut().unwrap().remove("device_name");
    let replaced = sign_in(&api, &body).await;

    assert_eq!(registered.status, 201, "{}", registered.json());
    assert_eq!(registered.json()["device_name"], "Ada's laptop");
    assert_eq!(phone.status, 201, "{}", phone.json());
    assert_eq!(phone_session.json()["device_name"], "Ada's phone");
    assert_eq!(replaced.status, 201, "{}", replaced.json());
    assert_eq!(replaced.json()["device_name"], json!(null));
    assert_eq!(
        api.database()
            .owner_reads("SELECT coalesce(device_name, '-') FROM sessions ORDER BY created_at")
            .await,
        ["Ada's laptop", "-"]
    );
    for (device_name, code) in [
        (" \t ".to_owned(), "required"),
        ("Ada's\u{0}laptop".to_owned(), "control_character"),
        ("é".repeat(101), "too_long"),
    ] {
        let mut body = grant();
        body["device_name"] = json!(device_name);

        let refused = api
            .request(http::Method::POST, "/v1/sessions/provider")
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await;

        assert_eq!(refused.status, 400, "{code}");
        assert_eq!(refused.json()["errors"][0]["field"], "/device_name");
        assert_eq!(refused.json()["errors"][0]["code"], code);
    }
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM sessions")
            .await,
        ["2"]
    );
}

#[tokio::test]
async fn auth_058_a_sign_in_through_a_provider_to_an_account_with_an_app_answers_a_challenge_that_holds_its_provider()
 {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let registered = sign_in(&api, &grant()).await;
    assert_eq!(registered.status, 201, "{}", registered.json());
    let ada = SignedIn::from(&registered);
    let device_token = registered.json()["device_token"]
        .as_str()
        .unwrap()
        .to_owned();
    second_factors::add_app(&api, &ada).await;
    let count = |table: &'static str| {
        let api = api.clone();
        async move {
            api.database()
                .owner_reads(&format!("SELECT count(*)::text FROM {table}"))
                .await
        }
    };
    let (sessions, tokens) = (count("sessions").await, count("device_tokens").await);
    let mut body = grant();
    body["device_id"] = json!("laptop");
    body["device_name"] = json!("Ada's laptop");
    body["device_token"] = json!(device_token);

    let response = sign_in(&api, &body).await;

    assert_eq!(response.status, 200, "{}", response.json());
    let answer = response.json();
    let token = answer["challenge"].as_str().unwrap();
    assert_eq!(token.len(), 43);
    assert!(answer.get("access_token").is_none(), "{answer}");
    assert!(answer.get("device_token").is_none(), "{answer}");
    assert_eq!(
        answer["expires_at"].as_str().unwrap().len(),
        "2026-11-11T12:00:00.000000Z".len()
    );
    assert_eq!(count("sessions").await, sessions, "no session");
    assert_eq!(count("device_tokens").await, tokens, "no device token");
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT (expires_at - created_at)::text || ' ' || device_id || ' ' || \
                 device_name || ' ' || \
                 (device_token_hash = sha256(convert_to('{device_token}', 'UTF8')))::text \
                 || ' ' || provider || ' ' || \
                 (token_hash = sha256(convert_to('{token}', 'UTF8')))::text \
                 FROM sign_in_challenges"
            ))
            .await,
        ["00:05:00 laptop Ada's laptop true stand_in true"]
    );
    let ada_id = &ada.user_id;
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || \
                 resource_type || ' ' || resource_id FROM audit_entries \
                 WHERE action = 'sign_in.challenged'"
            )
            .await,
        [format!("{ada_id} - user {ada_id}")]
    );
}

#[tokio::test]
async fn auth_058_a_browser_s_sign_in_through_a_provider_takes_the_challenge_in_a_cookie() {
    let upstream = Upstream::start().await;
    let api =
        Api::with(providers::stand_in(&upstream).set("CORS_ALLOWED_ORIGINS", users::WEB)).await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    second_factors::add_app(&api, &ada).await;

    let browser = api
        .post("/v1/sessions/provider", &grant())
        .header("origin", users::WEB)
        .send()
        .await;
    assert_eq!(browser.status, 200, "{}", browser.json());
    assert!(browser.json().get("challenge").is_none());
    let set = browser
        .headers("set-cookie")
        .into_iter()
        .find(|set| set.starts_with("__Host-sign_in_challenge="))
        .expect("the challenge's cookie")
        .to_owned();
    assert!(
        set.ends_with("; Path=/; Secure; HttpOnly; SameSite=Strict"),
        "{set}"
    );
    assert!(!set.contains("Domain"), "{set}");
    let cookie = users::cookie(&browser, "sign_in_challenge");
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT to_char(expires_at AT TIME ZONE 'UTC', \
                 'Dy, DD Mon YYYY HH24:MI:SS \"GMT\"') FROM sign_in_challenges \
                 WHERE token_hash = sha256(convert_to('{cookie}', 'UTF8'))"
            ))
            .await,
        [users::expires(&browser, "sign_in_challenge")]
    );
}

#[tokio::test]
async fn auth_027_a_registration_through_a_provider_that_proves_no_address_enqueues_the_job_of_the_first_code()
 {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", false));
    let off = api
        .instance(providers::stand_in(&upstream).set("EMAIL", "off"))
        .await;

    let registered = sign_in(&api, &grant()).await;
    gives(&upstream, identity("subject-2", "grace@example.com", false));
    let without_email = sign_in(&off, &grant()).await;

    assert_eq!(registered.status, 201, "{}", registered.json());
    assert_eq!(without_email.status, 201, "{}", without_email.json());
    assert_eq!(
        api.database()
            .owner_reads("SELECT kind || ' ' || user_id || ' ' || payload::text FROM jobs")
            .await,
        [format!(
            "auth.send_verification_code {} {{\"purpose\": \"email_verification\"}}",
            registered.json()["user_id"].as_str().unwrap()
        )],
        "with email off, the registration enqueues none"
    );
}

#[tokio::test]
async fn auth_026_a_sign_in_and_a_re_authentication_through_a_provider_say_whether_the_account_must_prove_its_address()
 {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", false));
    let registered = signed_in(&api).await;

    let signed_in_unproved = sign_in(&api, &grant()).await;
    let reauthenticated_unproved = reauthenticate(&api, &registered, json!({})).await;
    users::verified(&api, &registered.user_id).await;
    let signed_in_proved = sign_in(&api, &grant()).await;
    let reauthenticated_proved =
        reauthenticate(&api, &SignedIn::from(&reauthenticated_unproved), json!({})).await;
    // With email off, no account must prove its address.
    gives(&upstream, identity("subject-2", "grace@example.com", false));
    let off = api
        .instance(providers::stand_in(&upstream).set("EMAIL", "off"))
        .await;
    let signed_in_off = sign_in(&off, &grant()).await;
    let reauthenticated_off =
        reauthenticate(&off, &SignedIn::from(&signed_in_off), json!({})).await;

    for (response, required, what) in [
        (&signed_in_unproved, true, "a sign-in, unproved"),
        (
            &reauthenticated_unproved,
            true,
            "a re-authentication, unproved",
        ),
        (&signed_in_proved, false, "a sign-in, proved"),
        (
            &reauthenticated_proved,
            false,
            "a re-authentication, proved",
        ),
        (&signed_in_off, false, "a sign-in, email off"),
        (
            &reauthenticated_off,
            false,
            "a re-authentication, email off",
        ),
    ] {
        assert!(response.status.is_success(), "{what}: {}", response.json());
        assert_eq!(
            response.json()["email_verification_required"],
            required,
            "{what}"
        );
    }
}

#[tokio::test]
async fn auth_061_a_re_authentication_through_a_provider_that_carries_a_code_counts_in_the_user_s_bucket_of_checks_of_codes()
 {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    // The confirmation of the app's setup counts once in the bucket of 10.
    let app = second_factors::add_app(&api, &ada).await;
    let setup = second_factors::start_setup(&api, &ada).await;
    assert_eq!(setup.status, 200, "{}", setup.json());
    let other = setup.json()["secret"].as_str().unwrap().to_owned();
    let wrong = second_factors::other_code(&second_factors::code(&api, &other).await);
    for _ in 0..9 {
        let refused = second_factors::confirm(&api, &ada, &wrong).await;
        assert_eq!(refused.status, 422, "{}", refused.json());
    }
    let received = upstream.received().len();

    let right = second_factors::code(&api, &app.secret).await;
    let limited = reauthenticate(&api, &ada, json!({ "second_factor_code": right })).await;
    let exchanged = upstream.received().len();
    let password_checks = users::counted_in(&api, "password_check", &ada).await;
    let without_a_code = reauthenticate(&api, &ada, json!({})).await;

    assert_eq!(limited.status, 429, "{}", limited.json());
    assert_eq!(limited.json()["code"], "request.rate_limited");
    assert!(
        limited
            .header("ratelimit-policy")
            .starts_with("\"second_factor_check\";q=10;w=86400"),
        "{}",
        limited.header("ratelimit-policy")
    );
    assert_eq!(
        exchanged, received,
        "before it reads the code, it exchanges nothing"
    );
    assert_eq!(
        password_checks, None,
        "the bucket of codes refused it before the password's counted it"
    );
    // One that carries no code counts in no such bucket.
    assert_eq!(without_a_code.status, 403, "{}", without_a_code.json());
    assert_eq!(without_a_code.json()["code"], "auth.second_factor_required");
}

/// The requests for a token that the stand-in received.
fn exchanges(upstream: &Upstream) -> Vec<crate::support::upstream::Received> {
    upstream
        .received()
        .into_iter()
        .filter(|request| request.method == "POST" && request.path == "/token")
        .collect()
}

#[tokio::test]
async fn auth_077_a_re_authentication_exchanges_the_code_and_answers_a_provider_it_can_t_exchange_at_as_a_sign_in_does()
 {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    let before = exchanges(&upstream).len();
    let proved = reauthenticate(&api, &ada, json!({})).await;
    assert_eq!(proved.status, 200, "{}", proved.json());
    let exchange = &exchanges(&upstream)[before];
    assert_eq!(
        exchange.authorization, None,
        "the client's ID and secret go in the request's body"
    );
    for (field, value) in [
        ("grant_type", "authorization_code"),
        ("client_id", "stand-in-client"),
        ("client_secret", "stand-in-secret"),
        ("code", "a-code"),
        ("code_verifier", VERIFIER),
        ("redirect_uri", "https://app.example.com/callback"),
    ] {
        assert_eq!(exchange.field(field).as_deref(), Some(value), "{field}");
    }
    let ada = SignedIn::from(&proved);

    let unknown = reauthenticate(&api, &ada, json!({ "provider": "nobody" })).await;
    upstream.set("POST", "/token", 401, json!({ "error": "invalid_client" }));
    let refused_client = reauthenticate(&api, &ada, json!({})).await;
    upstream.set("POST", "/token", 500, json!({}));
    let failing = reauthenticate(&api, &ada, json!({})).await;

    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.json()["code"], "auth.provider_not_found");
    for response in [&refused_client, &failing] {
        assert_eq!(response.status, 503, "{}", response.json());
        assert_eq!(response.json()["code"], "auth.provider_unavailable");
        assert_eq!(response.header("retry-after"), "30");
    }
    let plain = Api::new().await;
    let grace = users::register(&plain, "grace@example.com").await;
    let unconfigured = reauthenticate(&plain, &grace, json!({})).await;
    assert_eq!(unconfigured.status, 503);
    assert_eq!(unconfigured.json()["code"], "request.feature_disabled");
}

#[tokio::test]
async fn auth_100_a_re_authentication_refuses_a_malformed_body_before_the_exchange_and_answers_503_at_its_deadline()
 {
    let (upstream, api) = standing().await;
    let bob = users::register(&api, "bob@example.com").await;
    let malformed = api
        .post_raw(
            REAUTHENTICATE,
            &json!({
                "provider": "stand_in",
                "code": "a code with spaces",
                "code_verifier": "too-short",
                "redirect_uri": "https://evil.example/callback",
            }),
        )
        .bearer(&bob.access)
        .send()
        .await;
    assert_eq!(malformed.status, 400);
    assert_eq!(
        refused_fields(&malformed),
        [
            "/code invalid_code",
            "/code_verifier invalid_code_verifier",
            "/redirect_uri invalid_redirect_uri"
        ]
    );
    assert!(upstream.received().is_empty(), "before the exchange");

    upstream.hang("POST", "/token");
    let clock = api.clock();
    let call = {
        let api = api.clone();
        let access = bob.access.clone();
        tokio::spawn(async move {
            api.post(REAUTHENTICATE, &grant())
                .bearer(&access)
                .send()
                .await
        })
    };
    clock.sleeping(std::time::Duration::from_secs(6)).await;
    clock.advance(std::time::Duration::from_secs(6));
    let hung = call.await.unwrap();
    assert_eq!(hung.status, 503);
    assert_eq!(hung.json()["code"], "auth.provider_unavailable");
}

/// The path of a re-authentication through a provider.
const REAUTHENTICATE: &str = "/v1/users/me/sessions/current/reauthenticate/provider";

/// The path of a link of a provider.
const LINK: &str = "/v1/users/me/identities/provider";

/// The access token of a caller of `path`: `ada`'s, for a re-authentication, whose subject the
/// stand-in gives, and, for a link, that of a new user, the `n`th, for whom the stand-in gives a
/// subject of their own.
async fn caller_of(api: &Api, upstream: &Upstream, path: &str, ada: &SignedIn, n: usize) -> String {
    if path == REAUTHENTICATE {
        let claims = identity("subject-1", "ada@example.com", true);
        upstream.set("GET", "/identity", 200, claims);
        return ada.access.clone();
    }
    let email = format!("user-{n}@example.com");
    let claims = identity(&format!("subject-of-user-{n}"), &email, true);
    upstream.set("GET", "/identity", 200, claims);
    users::register(api, &email).await.access
}

#[tokio::test]
async fn auth_078_a_re_authentication_and_a_link_take_a_listed_or_loopback_redirect_uri_which_names_the_client()
 {
    let (upstream, api) = standing_with_both_clients().await;
    upstream.set("DELETE", "/token", 204, json!({}));
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let mut ada = signed_in(&api).await;
    let mut callers = 0;
    for path in [REAUTHENTICATE, LINK] {
        for (redirect_uri, client_id) in [
            ("http://127.0.0.1:51234/callback", Some("stand-in-client")),
            (SECOND_REDIRECT_URI, Some("second-client")),
            ("http://127.0.0.2:51234/callback", None),
            ("https://app.example.com/callback/other", None),
        ] {
            callers += 1;
            let access = caller_of(&api, &upstream, path, &ada, callers).await;
            let mut body = grant();
            body["redirect_uri"] = json!(redirect_uri);
            let before = exchanges(&upstream).len();

            let response = api.post_raw(path, &body).bearer(&access).send().await;

            let exchanged = exchanges(&upstream);
            let Some(client_id) = client_id else {
                assert_eq!(response.status, 400, "{path} {redirect_uri}");
                assert_eq!(response.json()["errors"][0]["field"], "/redirect_uri");
                assert_eq!(response.json()["errors"][0]["code"], "invalid_redirect_uri");
                assert_eq!(exchanged.len(), before, "{path}: before the exchange");
                continue;
            };
            assert_eq!(
                response.status,
                200,
                "{path} {redirect_uri}: {}",
                response.json()
            );
            let exchange = &exchanged[before];
            assert_eq!(
                exchange.field("redirect_uri").as_deref(),
                Some(redirect_uri),
                "{path}"
            );
            assert_eq!(
                exchange.field("client_id").as_deref(),
                Some(client_id),
                "{path}"
            );
            if path == REAUTHENTICATE {
                // With the tokens that the proof gave the session (AUTH-095).
                ada = SignedIn::from(&response);
            }
        }
    }
}

/// Sends `body` to `path` for the `n`th caller of [`caller_of`], with the stand-in's userinfo
/// endpoint failing unless `reads` says it answers, and keeps the tokens that a re-authentication
/// of `ada`'s session gives it (AUTH-095).
async fn prove(
    (api, upstream): (&Api, &Upstream),
    path: &str,
    ada: &mut SignedIn,
    (n, body, reads): (usize, &Value, bool),
) -> Response {
    let access = caller_of(api, upstream, path, ada, n).await;
    if !reads {
        upstream.set("GET", "/identity", 500, json!({}));
    }
    let response = api.post(path, body).bearer(&access).send().await;
    if path == REAUTHENTICATE && response.status == 200 {
        *ada = SignedIn::from(&response);
    }
    response
}

#[tokio::test]
async fn auth_079_a_re_authentication_and_a_link_keep_the_provider_s_token_nowhere_and_delete_one_that_doesn_t_lapse()
 {
    let (upstream, api) = standing_with_both_clients().await;
    let token = "a-token-that-no-log-of-a-proof-holds";
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let mut ada = signed_in(&api).await;
    upstream.set("POST", "/token", 200, json!({ "access_token": token }));
    let bearer = Some(format!("Bearer {token}"));
    let mut callers = 0;
    for path in [REAUTHENTICATE, LINK] {
        upstream.set("DELETE", "/token", 204, json!({}));
        let start = deletions(&upstream).len();
        let since = |upstream: &Upstream| deletions(upstream)[start..].to_vec();
        let mut next = || {
            callers += 1;
            callers
        };

        // The first client's tokens lapse, and the second's don't.
        let lapsing = prove((&api, &upstream), path, &mut ada, (next(), &grant(), true)).await;
        let after_lapsing = since(&upstream);
        let read = prove(
            (&api, &upstream),
            path,
            &mut ada,
            (next(), &second_grant(), true),
        )
        .await;
        let after_read = since(&upstream);
        let unread = prove(
            (&api, &upstream),
            path,
            &mut ada,
            (next(), &second_grant(), false),
        )
        .await;
        let after_unread = since(&upstream);
        upstream.set("DELETE", "/token", 500, json!({}));
        let kept = prove(
            (&api, &upstream),
            path,
            &mut ada,
            (next(), &second_grant(), true),
        )
        .await;

        assert_eq!(lapsing.status, 200, "{path}: {}", lapsing.json());
        assert!(after_lapsing.is_empty(), "{path}: {after_lapsing:?}");
        assert!(
            !String::from_utf8_lossy(&lapsing.body).contains(token),
            "{path}: no answer holds it"
        );
        assert_eq!(read.status, 200, "{path}: {}", read.json());
        assert_eq!(after_read, std::slice::from_ref(&bearer), "{path}");
        assert_eq!(unread.status, 503, "{path}");
        assert_eq!(after_unread, [bearer.clone(), bearer.clone()], "{path}");
        assert_eq!(
            kept.status,
            200,
            "{path}: a failed deletion refuses nothing: {}",
            kept.json()
        );
        assert!(
            kept_token_logged(&kept, "answered 500"),
            "{path}: ERR-11's line"
        );
    }
    let found = api
        .database()
        .owner_reads(&format!(
            "SELECT count(*)::text FROM (\
               SELECT details::text AS value FROM audit_entries \
               UNION ALL SELECT payload::text FROM jobs \
               UNION ALL SELECT payload::text FROM events \
               UNION ALL SELECT subject FROM provider_identities) AS stored \
             WHERE value LIKE '%{token}%'"
        ))
        .await;
    assert_eq!(found, ["0"], "no row holds it");
    assert!(!logged(token), "no log holds it");
}
