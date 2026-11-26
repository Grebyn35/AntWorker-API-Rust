//! GitHub, through OAuth 2.0 (AUTH-083, AUTH-102), against a stand-in of its endpoints.

use serde_json::{Value, json};

use crate::support::{
    Api, Options, api,
    api::Response,
    providers::{self, VERIFIER, kept_token_logged},
    upstream::Upstream,
};

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
    providers::sign_in(
        api,
        &json!({
            "provider": "github",
            "code": "a-code",
            "code_verifier": VERIFIER,
            "redirect_uri": "https://app.example.com/callback/github",
        }),
    )
    .await
}

/// GitHub's list of an account's addresses, whose primary one is `ada@example.com`.
fn primary() -> Value {
    json!([
        { "email": "old@example.com", "primary": false, "verified": true, "visibility": null },
        { "email": "ada@example.com", "primary": true, "verified": true, "visibility": "private" },
    ])
}

#[tokio::test]
async fn auth_083_github_signs_in_with_its_numeric_id_and_primary_address_and_deletes_its_token() {
    let (upstream, api) = github().await;
    gives(
        &upstream,
        json!({ "id": 583231, "login": "octo-ada", "name": null }),
        primary(),
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
    // GitHub's 204 is a deletion that succeeded, which ERR-11 logs no line of.
    assert!(!kept_token_logged(&response, ""), "a line of a token kept");
}

#[tokio::test]
async fn auth_083_github_s_name_names_the_account_before_its_login() {
    let (upstream, api) = github().await;
    gives(
        &upstream,
        json!({ "id": 583231, "login": "octo-ada", "name": "Ada Lovelace" }),
        primary(),
    );

    let response = sign_in(&api).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert_eq!(
        api.database().owner_reads("SELECT name FROM users").await,
        ["Ada Lovelace"]
    );
}

#[tokio::test]
async fn auth_083_a_deletion_that_fails_refuses_nothing() {
    let (upstream, api) = github().await;
    gives(
        &upstream,
        json!({ "id": 1, "login": "ada", "name": "Ada" }),
        primary(),
    );
    upstream.set(
        "DELETE",
        "/applications/github-client/token",
        500,
        json!({}),
    );

    let response = sign_in(&api).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert!(
        kept_token_logged(&response, "answered 500"),
        "ERR-11's line"
    );
}

// part: files
#[tokio::test]
async fn auth_092_github_gives_its_user_s_picture_for_the_new_account() {
    let (upstream, api) = github().await;
    let picture = "https://avatars.githubusercontent.com/u/583231?v=4";
    gives(
        &upstream,
        json!({ "id": 583231, "login": "octo-ada", "name": null, "avatar_url": picture }),
        primary(),
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
async fn auth_083_a_refusal_other_than_of_the_client_answers_as_one_of_the_code() {
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
}

#[tokio::test]
async fn auth_083_a_refusal_of_the_client_answers_as_one_of_the_api_s_own_client() {
    let (upstream, api) = github().await;
    upstream.set(
        "POST",
        "/login/oauth/access_token",
        200,
        json!({ "error": "incorrect_client_credentials" }),
    );

    let response = sign_in(&api).await;

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "auth.provider_unavailable");
}

#[tokio::test]
async fn auth_083_a_deletion_that_doesn_t_end_in_time_refuses_nothing() {
    let (upstream, api) = github().await;
    gives(
        &upstream,
        json!({ "id": 7, "login": "ada", "name": "Ada" }),
        primary(),
    );
    upstream.hang("DELETE", "/applications/github-client/token");
    let clock = api.clock();
    let call = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api).await })
    };

    // The deletion's deadline waits once its request is sent, so the clock moves only then.
    while !upstream
        .received()
        .iter()
        .any(|received| received.method == "DELETE")
    {
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

#[tokio::test]
async fn auth_102_startup_refuses_another_client_s_redirect_uri_under_one_of_github_s() {
    let beside = |stand_in: &str| {
        Options::default()
            .set("GITHUB_CLIENT_ID", "github-client")
            .set("GITHUB_CLIENT_SECRET", "github-secret")
            .set("GITHUB_REDIRECT_URIS", "https://app.example.com/callback")
            .set("STAND_IN_NAME", "Stand-in")
            .set("STAND_IN_CLIENT_ID", "stand-in-client")
            .set("STAND_IN_CLIENT_SECRET", "stand-in-secret")
            .set("STAND_IN_REDIRECT_URIS", stand_in)
    };

    for under in [
        "https://app.example.com/callback/stand-in",
        "https://sign-in.app.example.com/callback/stand-in",
    ] {
        let refusal = api::refusal(&beside(under));
        assert!(
            refusal.starts_with("STAND_IN_REDIRECT_URIS must be"),
            "{under}: {refusal}"
        );
    }
    api::configuration(&beside("https://app.example.com/stand-in/callback"));
}
