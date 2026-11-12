//! GitHub, through OAuth 2.0 (AUTH-083), against a stand-in of its endpoints.

use serde_json::{Value, json};

use platform::telemetry::testing;

use crate::support::{Api, api::Response, upstream::Upstream};

/// A verifier of a PKCE challenge, of the 43 characters that RFC 7636 asks at least.
const VERIFIER: &str = "a-verifier-of-at-least-43-characters.0123456789";

/// A stand-in of GitHub's endpoints, and an API with GitHub's client.
async fn github() -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let options = upstream
        .options()
        .set("GITHUB_CLIENT_ID", "github-client")
        .set("GITHUB_CLIENT_SECRET", "github-secret")
        .set(
            "GITHUB_REDIRECT_URIS",
            "https://app.example.com/callback/github,http://127.0.0.1/callback/github",
        );
    let api = Api::with(options).await;
    (upstream, api)
}

/// Makes GitHub give a token, `user` for it, and `emails`, and delete the token.
fn gives(upstream: &Upstream, user: Value, emails: Value) {
    upstream.set(
        "POST",
        "/login/oauth/access_token",
        200,
        json!({ "access_token": "github-token", "token_type": "bearer", "scope": "user:email" }),
    );
    upstream.set("GET", "/user", 200, user);
    upstream.set("GET", "/user/emails", 200, emails);
    upstream.set(
        "DELETE",
        "/applications/github-client/token",
        204,
        json!(null),
    );
}

async fn sign_in(api: &Api) -> Response {
    api.post(
        "/v1/sessions/provider",
        &json!({
            "provider": "github",
            "code": "a-code",
            "code_verifier": VERIFIER,
            "redirect_uri": "https://app.example.com/callback/github",
        }),
    )
    .send()
    .await
}

fn primary(email: &str) -> Value {
    json!([
        { "email": "old@example.com", "primary": false, "verified": true, "visibility": null },
        { "email": email, "primary": true, "verified": true, "visibility": "private" },
    ])
}

#[tokio::test]
async fn auth_083_github_signs_in_with_its_numeric_id_and_primary_address_and_deletes_its_token() {
    let (upstream, api) = github().await;
    gives(
        &upstream,
        json!({ "id": 583231, "login": "octo-ada", "name": null }),
        primary("ada@example.com"),
    );

    let response = sign_in(&api).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(
        response.json()["email_verification_required"],
        true,
        "GitHub proves no address"
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT email || ' ' || name FROM users")
            .await,
        ["ada@example.com octo-ada"]
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT subject FROM provider_identities WHERE provider = 'github'")
            .await,
        ["583231"]
    );
    let received = upstream.received();
    let exchange = &received[0];
    assert_eq!(
        exchange.field("client_secret").as_deref(),
        Some("github-secret")
    );
    assert_eq!(exchange.field("code_verifier").as_deref(), Some(VERIFIER));
    let deletion = received.last().unwrap();
    assert_eq!(
        (deletion.method.as_str(), deletion.path.as_str()),
        ("DELETE", "/applications/github-client/token")
    );
    assert!(
        deletion
            .authorization
            .as_deref()
            .is_some_and(|value| value.starts_with("Basic ")),
        "{deletion:?}"
    );
    assert_eq!(deletion.body, r#"{"access_token":"github-token"}"#);
}

#[tokio::test]
async fn auth_083_a_deletion_that_fails_refuses_nothing() {
    let (upstream, api) = github().await;
    gives(
        &upstream,
        json!({ "id": 1, "login": "ada", "name": "Ada" }),
        primary("ada@example.com"),
    );
    upstream.set(
        "DELETE",
        "/applications/github-client/token",
        500,
        json!({}),
    );

    let response = sign_in(&api).await;

    assert_eq!(response.status, 201, "{}", response.json());
    let logged = testing::all_logged()
        .into_iter()
        .filter(|line| line["error.type"] == "provider_token_not_deleted")
        .count();
    assert!(logged >= 1, "ERR-11's line");
    let counted: f64 = testing::recorded("platform.errors_ignored")
        .into_iter()
        .filter(|point| {
            point.labels.get("error.type").map(String::as_str) == Some("provider_token_not_deleted")
        })
        .map(|point| point.value)
        .sum();
    assert!(counted >= 1.0, "ERR-11's count");
}

#[tokio::test]
async fn auth_083_a_token_that_may_not_read_the_addresses_gives_none() {
    let (upstream, api) = github().await;
    gives(
        &upstream,
        json!({ "id": 1, "login": "ada", "name": "Ada" }),
        json!({}),
    );
    upstream.set(
        "GET",
        "/user/emails",
        404,
        json!({ "message": "Not Found" }),
    );

    let response = sign_in(&api).await;

    assert_eq!(response.status, 422);
    assert_eq!(response.json()["code"], "auth.provider_address_missing");
    assert_eq!(
        upstream.received().last().unwrap().method,
        "DELETE",
        "the token is deleted whatever the outcome"
    );
}

#[tokio::test]
async fn auth_083_a_refusal_of_the_client_is_the_api_s_and_any_other_the_code_s() {
    let (upstream, api) = github().await;
    upstream.set(
        "POST",
        "/login/oauth/access_token",
        200,
        json!({ "error": "bad_verification_code" }),
    );
    let response = sign_in(&api).await;
    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "auth.provider_code_rejected");

    upstream.set(
        "POST",
        "/login/oauth/access_token",
        200,
        json!({ "error": "incorrect_client_credentials" }),
    );
    let response = sign_in(&api).await;
    assert_eq!(response.status, 503);
}
