//! Google, through OpenID Connect (AUTH-082), against a stand-in of its endpoints.

use serde_json::{Value, json};

use crate::support::{Api, api::Response, providers::VERIFIER, upstream::Upstream};

/// A stand-in of Google's endpoints, and an API with both of Google's clients.
async fn google() -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let options = upstream
        .options()
        .set("GOOGLE_CLIENT_ID", "google-web")
        .set("GOOGLE_CLIENT_SECRET", "web-secret")
        .set(
            "GOOGLE_REDIRECT_URIS",
            "https://app.example.com/callback/google",
        )
        .set("GOOGLE_DESKTOP_CLIENT_ID", "google-desktop")
        .set("GOOGLE_DESKTOP_CLIENT_SECRET", "desktop-secret")
        .set(
            "GOOGLE_DESKTOP_REDIRECT_URIS",
            "http://127.0.0.1/callback/google",
        );
    let api = Api::with(options).await;
    (upstream, api)
}

/// Makes Google's token endpoint give a token, and its userinfo endpoint `info`.
fn gives(upstream: &Upstream, info: Value) {
    upstream.set(
        "POST",
        "/token",
        200,
        json!({ "access_token": "google-token", "expires_in": 3599, "token_type": "Bearer" }),
    );
    upstream.set("GET", "/v1/userinfo", 200, info);
}

async fn sign_in(api: &Api, redirect_uri: &str) -> Response {
    api.post(
        "/v1/sessions/provider",
        &json!({
            "provider": "google",
            "code": "a-code",
            "code_verifier": VERIFIER,
            "redirect_uri": redirect_uri,
        }),
    )
    .send()
    .await
}

#[tokio::test]
async fn auth_082_google_signs_in_through_its_userinfo_with_its_browser_client() {
    let (upstream, api) = google().await;
    gives(
        &upstream,
        json!({ "sub": "1098", "email": "ada@gmail.com", "email_verified": true, "name": "Ada" }),
    );

    let response = sign_in(&api, "https://app.example.com/callback/google").await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(response.json()["email_verification_required"], false);
    let received = upstream.received();
    let exchange = &received[0];
    assert_eq!(exchange.path, "/token");
    assert_eq!(
        exchange.field("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(exchange.field("client_id").as_deref(), Some("google-web"));
    assert_eq!(
        exchange.field("client_secret").as_deref(),
        Some("web-secret")
    );
    assert_eq!(received[1].path, "/v1/userinfo");
    assert_eq!(
        received[1].authorization.as_deref(),
        Some("Bearer google-token")
    );
    assert_eq!(received.len(), 2, "no revocation");
    assert_eq!(
        api.database()
            .owner_reads("SELECT subject FROM provider_identities WHERE provider = 'google'")
            .await,
        ["1098"]
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT email || ' ' || name FROM users")
            .await,
        ["ada@gmail.com Ada"],
        "the address and the name that the userinfo endpoint answers"
    );
}

#[tokio::test]
async fn auth_082_a_loopback_redirect_exchanges_through_the_desktop_client() {
    let (upstream, api) = google().await;
    gives(
        &upstream,
        json!({ "sub": "1098", "email": "ada@example.com", "email_verified": true, "hd": "example.com" }),
    );

    let response = sign_in(&api, "http://127.0.0.1:50123/callback/google").await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(response.json()["email_verification_required"], false, "hd");
    let exchange = &upstream.received()[0];
    assert_eq!(
        exchange.field("client_id").as_deref(),
        Some("google-desktop")
    );
    assert_eq!(
        exchange.field("client_secret").as_deref(),
        Some("desktop-secret")
    );
}

#[tokio::test]
async fn auth_082_an_address_google_doesn_t_host_is_a_claim() {
    let (upstream, api) = google().await;
    gives(
        &upstream,
        json!({ "sub": "1098", "email": "ada@example.com", "email_verified": true }),
    );

    let response = sign_in(&api, "https://app.example.com/callback/google").await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(response.json()["email_verification_required"], true);
}

#[tokio::test]
async fn auth_082_google_is_listed_by_its_key_name_scopes_and_two_clients() {
    let (_upstream, api) = google().await;

    let capabilities = api.get("/v1/capabilities").send().await.json();

    let google = &capabilities["sign_in_providers"][0];
    assert_eq!(google["provider"], "google");
    assert_eq!(google["name"], "Google");
    assert_eq!(google["scopes"], json!(["openid", "email", "profile"]));
    assert_eq!(
        google["authorization_endpoint"],
        "https://accounts.google.com/o/oauth2/v2/auth"
    );
    let clients: Vec<&str> = google["clients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|client| client["client_id"].as_str().unwrap())
        .collect();
    assert_eq!(clients, ["google-web", "google-desktop"]);
}
