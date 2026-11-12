//! An OpenID Connect provider that the deployment names in its settings (AUTH-084), against a
//! stand-in of its endpoints.

use serde_json::{Value, json};

use crate::support::{
    Api,
    api::{self, Options, Response},
    providers::VERIFIER,
    upstream::Upstream,
};

const ISSUER: &str = "https://login.example.com";
const REDIRECT_URI: &str = "https://app.example.com/callback/login";

/// The settings that name a provider, without those of `upstream`.
fn settings(options: Options, issuer: &str) -> Options {
    options
        .set("OPENID_CONNECT_CLIENT_ID", "named-client")
        .set("OPENID_CONNECT_CLIENT_SECRET", "named+secret/1")
        .set("OPENID_CONNECT_REDIRECT_URIS", REDIRECT_URI)
        .set("OPENID_CONNECT_NAME", "Example login")
        .set("OPENID_CONNECT_ISSUER", issuer)
        .set(
            "OPENID_CONNECT_AUTHORIZATION_ENDPOINT",
            "https://login.example.com/oauth2/authorize",
        )
        .set(
            "OPENID_CONNECT_TOKEN_ENDPOINT",
            "https://login.example.com/oauth2/token",
        )
        .set(
            "OPENID_CONNECT_USERINFO_ENDPOINT",
            "https://login.example.com/oauth2/userinfo",
        )
}

/// A stand-in of the provider's endpoints, and an API whose settings name it.
async fn named() -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let api = Api::with(settings(upstream.options(), ISSUER)).await;
    (upstream, api)
}

/// Makes the provider's token endpoint give a token, and its userinfo endpoint `info`.
fn gives(upstream: &Upstream, info: Value) {
    upstream.set(
        "POST",
        "/oauth2/token",
        200,
        json!({ "access_token": "named-token", "expires_in": 300, "token_type": "Bearer" }),
    );
    upstream.set("GET", "/oauth2/userinfo", 200, info);
}

async fn sign_in(api: &Api) -> Response {
    api.post(
        "/v1/sessions/provider",
        &json!({
            "provider": "openid_connect",
            "code": "a-code",
            "code_verifier": VERIFIER,
            "redirect_uri": REDIRECT_URI,
        }),
    )
    .send()
    .await
}

async fn subjects(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT subject FROM provider_identities WHERE provider = 'openid_connect' \
             ORDER BY created_at",
        )
        .await
}

#[tokio::test]
async fn auth_084_the_provider_that_the_settings_name_signs_in_through_their_addresses() {
    let (upstream, api) = named().await;
    gives(
        &upstream,
        json!({ "sub": "7", "email": "ada@example.com", "email_verified": true, "name": "Ada" }),
    );

    let response = sign_in(&api).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(
        response.json()["email_verification_required"],
        true,
        "it proves no address, whatever it marks verified"
    );
    let received = upstream.received();
    assert_eq!(received[0].path, "/oauth2/token");
    assert_eq!(
        received[0].authorization.as_deref(),
        // The Basic scheme's Base64 of `named-client:named%2Bsecret%2F1`, the ID and the secret
        // as a form encodes them (RFC 6749, §2.3.1).
        Some("Basic bmFtZWQtY2xpZW50Om5hbWVkJTJCc2VjcmV0JTJGMQ==")
    );
    assert_eq!(received[0].field("client_id"), None);
    assert_eq!(received[0].field("client_secret"), None);
    assert_eq!(received[0].field("code").as_deref(), Some("a-code"));
    assert_eq!(received[1].path, "/oauth2/userinfo");
    assert_eq!(
        received[1].authorization.as_deref(),
        Some("Bearer named-token")
    );
    assert_eq!(received.len(), 2, "no deletion of the token");
    assert_eq!(subjects(&api).await, ["https://login.example.com 7"]);
    assert_eq!(
        api.database()
            .owner_reads("SELECT email || ' ' || name FROM users")
            .await,
        ["ada@example.com Ada"],
        "the address and the name that the userinfo endpoint answers"
    );
}

#[tokio::test]
async fn auth_084_a_token_whose_answer_doesn_t_say_that_it_lapses_answers_503() {
    let (upstream, api) = named().await;
    upstream.set(
        "GET",
        "/oauth2/userinfo",
        200,
        json!({ "sub": "7", "email": "ada@example.com" }),
    );
    for token in [
        json!({ "access_token": "named-token", "token_type": "Bearer" }),
        json!({ "access_token": "named-token", "expires_in": 0, "token_type": "Bearer" }),
    ] {
        upstream.set("POST", "/oauth2/token", 200, token.clone());

        let response = sign_in(&api).await;

        assert_eq!(response.status, 503, "{token}");
        assert_eq!(response.json()["code"], "auth.provider_unavailable");
    }
    assert!(
        upstream
            .received()
            .iter()
            .all(|request| request.path == "/oauth2/token"),
        "no read of the identity with a token that may not lapse"
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM users")
            .await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_084_the_same_sub_of_another_issuer_reaches_no_account_of_the_first_s() {
    let (upstream, api) = named().await;
    gives(&upstream, json!({ "sub": "7", "email": "ada@example.com" }));
    assert_eq!(sign_in(&api).await.status, 201);

    // The deployment names another provider, which gives one of its users the same `sub`.
    let other = api
        .instance(settings(upstream.options(), "https://other.example.com"))
        .await;
    gives(
        &upstream,
        json!({ "sub": "7", "email": "grace@example.com" }),
    );
    let response = sign_in(&other).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(
        subjects(&other).await,
        ["https://login.example.com 7", "https://other.example.com 7"]
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM users")
            .await,
        ["2"],
        "a second account, not the first's"
    );

    // With the first user's address, it joins no account either (AUTH-076).
    gives(&upstream, json!({ "sub": "8", "email": "ada@example.com" }));
    let response = sign_in(&other).await;
    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "auth.email_in_use");
}

#[tokio::test]
async fn auth_084_it_is_listed_by_its_key_with_the_name_and_the_address_of_its_settings() {
    let (_upstream, api) = named().await;

    let capabilities = api.get("/v1/capabilities").send().await.json();

    assert_eq!(
        capabilities["sign_in_providers"],
        json!([{
            "provider": "openid_connect",
            "name": "Example login",
            "authorization_endpoint": "https://login.example.com/oauth2/authorize",
            "scopes": ["openid", "email", "profile"],
            "clients": [{
                "client_id": "named-client",
                "redirect_uris": [REDIRECT_URI],
            }],
        }])
    );
}

#[tokio::test]
async fn auth_084_its_five_settings_are_set_with_its_client_and_only_with_it() {
    let alone = Options::default().set("OPENID_CONNECT_NAME", "Example login");
    assert_eq!(api::refusal(&alone), "OPENID_CONNECT_CLIENT_ID is required");

    for missing in [
        "OPENID_CONNECT_NAME",
        "OPENID_CONNECT_ISSUER",
        "OPENID_CONNECT_AUTHORIZATION_ENDPOINT",
        "OPENID_CONNECT_TOKEN_ENDPOINT",
        "OPENID_CONNECT_USERINFO_ENDPOINT",
    ] {
        let without = settings(Options::default(), ISSUER).set(missing, "");
        assert_eq!(api::refusal(&without), format!("{missing} is required"));
    }
    api::configuration(&settings(Options::default(), ISSUER));
}

#[tokio::test]
async fn auth_084_startup_refuses_an_issuer_or_an_address_that_isn_t_an_https_url() {
    let named = || settings(Options::default(), ISSUER);
    for wrong in [
        "http://login.example.com",
        "https://login.example.com?tenant=1",
        "https://login.example.com#top",
        "login.example.com",
        "https://login.example.com /realm",
    ] {
        assert_eq!(
            api::refusal(&named().set("OPENID_CONNECT_ISSUER", wrong)),
            "OPENID_CONNECT_ISSUER must be an https URL without a query or a fragment (AUTH-084)",
            "{wrong}"
        );
    }
    for endpoint in [
        "OPENID_CONNECT_AUTHORIZATION_ENDPOINT",
        "OPENID_CONNECT_TOKEN_ENDPOINT",
        "OPENID_CONNECT_USERINFO_ENDPOINT",
    ] {
        for wrong in [
            "http://login.example.com/oauth2",
            "https://login.example.com/oauth2#top",
            "/oauth2",
        ] {
            assert_eq!(
                api::refusal(&named().set(endpoint, wrong)),
                format!("{endpoint} must be an https URL without a fragment (AUTH-084)"),
                "{wrong}"
            );
        }
        // An endpoint may hold a query, as RFC 6749 allows (§3.1, §3.2).
        api::configuration(&named().set(endpoint, "https://login.example.com/oauth2?tenant=1"));
    }
}

#[tokio::test]
async fn auth_084_startup_refuses_a_name_longer_than_100_characters() {
    let named = |name: &str| settings(Options::default(), ISSUER).set("OPENID_CONNECT_NAME", name);

    assert_eq!(
        api::refusal(&named(&"n".repeat(101))),
        "OPENID_CONNECT_NAME must be a name of 1 to 100 characters (AUTH-084)"
    );
    api::configuration(&named(&"n".repeat(100)));
}
