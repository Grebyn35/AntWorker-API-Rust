//! GitHub, through OAuth 2.0 (AUTH-083), against a stand-in of its endpoints.

use serde_json::{Value, json};

use platform::telemetry::testing;

use crate::support::{Api, api::Response, providers::VERIFIER, upstream::Upstream};

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

// part: files
#[tokio::test]
async fn auth_083_github_gives_its_user_s_picture_for_the_new_account() {
    let (upstream, api) = github().await;
    let picture = "https://avatars.example.com/u/583231";
    gives(
        &upstream,
        json!({ "id": 583231, "login": "octo-ada", "name": null, "avatar_url": picture }),
        primary("ada@example.com"),
    );

    let response = sign_in(&api).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(
        api.database()
            .owner_reads("SELECT payload->>'url' FROM jobs WHERE kind = 'accounts.copy_picture'")
            .await,
        [picture]
    );
}
// end part: files

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

#[tokio::test]
async fn auth_083_a_deletion_that_doesn_t_end_in_time_refuses_nothing() {
    let (upstream, api) = github().await;
    gives(
        &upstream,
        json!({ "id": 7, "login": "ada", "name": "Ada" }),
        primary("ada@example.com"),
    );
    upstream.hang("DELETE", "/applications/github-client/token");
    let clock = api.clock();
    let call = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api).await })
    };

    // The deletion's deadline waits once its request is sent, so the clock moves only then.
    for _ in 0..100_000 {
        if upstream
            .received()
            .iter()
            .any(|received| received.method == "DELETE")
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    clock.sleeping(std::time::Duration::from_secs(2)).await;
    clock.advance(std::time::Duration::from_secs(2));

    let response = call.await.unwrap();
    assert_eq!(response.status, 201, "{}", response.json());
    assert!(
        testing::all_logged().iter().any(|line| {
            line["error.type"] == "provider_token_not_deleted"
                && line["exception.message"]
                    .as_str()
                    .is_some_and(|cause| cause.contains("no answer within 2 seconds"))
        }),
        "ERR-11's line"
    );
}

#[tokio::test]
async fn auth_083_github_is_listed_by_its_key_name_and_scopes() {
    let (_upstream, api) = github().await;

    let capabilities = api.get("/v1/capabilities").send().await.json();

    let github = &capabilities["sign_in_providers"][0];
    assert_eq!(github["provider"], "github");
    assert_eq!(github["name"], "GitHub");
    assert_eq!(github["scopes"], json!(["user:email"]));
    assert_eq!(
        github["authorization_endpoint"],
        "https://github.com/login/oauth/authorize"
    );
    assert_eq!(github["clients"][0]["client_id"], "github-client");
}
