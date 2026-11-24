//! The tests' stand-in provider: the settings of an API that signs in through it, what it gives,
//! and the requests that sign in and re-authenticate through it (the auth spec's "Signing in
//! through a provider").

use platform::telemetry::testing;
use serde_json::{Value, json};

use super::{
    Api,
    api::{Options, Response},
    upstream::Upstream,
    users::SignedIn,
    with_fields,
};

/// A verifier of a PKCE challenge, of the 43 characters that RFC 7636 asks at least.
pub const VERIFIER: &str = "a-verifier-of-at-least-43-characters.0123456789";

/// The stand-in's browser redirect URI, and its loopback one.
pub const REDIRECT_URIS: &str = "https://app.example.com/callback,http://127.0.0.1/callback";

/// The options of an API whose stand-in provider has one client, and its name, which answers at
/// `upstream`.
pub fn stand_in(upstream: &Upstream) -> Options {
    upstream
        .options()
        .set("STAND_IN_NAME", "Stand-in")
        .set("STAND_IN_CLIENT_ID", "stand-in-client")
        .set("STAND_IN_CLIENT_SECRET", "stand-in-secret")
        .set("STAND_IN_REDIRECT_URIS", REDIRECT_URIS)
}

/// A stand-in provider, and an API that signs in through it.
pub async fn standing() -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let api = Api::with(stand_in(&upstream)).await;
    (upstream, api)
}

/// Makes the stand-in give a token for any code, and the claims `identity` for it.
pub fn gives(upstream: &Upstream, identity: Value) {
    upstream.set(
        "POST",
        "/token",
        200,
        json!({ "access_token": "a-provider-token" }),
    );
    upstream.set("GET", "/identity", 200, identity);
}

/// The claims of the user whose `sub` is `subject`, with `email`, which the provider proves when
/// `proved` says so.
pub fn identity(subject: &str, email: &str, proved: bool) -> Value {
    json!({ "sub": subject, "email": email, "proved": proved, "name": "Ada Lovelace" })
}

/// The body of a sign-in through the stand-in, with its browser redirect URI.
pub fn grant() -> Value {
    json!({
        "provider": "stand_in",
        "code": "a-code",
        "code_verifier": VERIFIER,
        "redirect_uri": "https://app.example.com/callback",
    })
}

/// Keys outside the schema of a request's `provider`: none, 65 characters, and keys with a
/// capital, a digit first, a hyphen, and a space after the stand-in's key (AUTH-100).
pub fn keys_outside_the_schema() -> Vec<String> {
    ["", "Stand_in", "9lives", "stand-in", "stand_in "]
        .map(str::to_owned)
        .into_iter()
        .chain([format!("a{}", "b".repeat(64))])
        .collect()
}

pub async fn sign_in(api: &Api, body: &Value) -> Response {
    api.post("/v1/sessions/provider", body).send().await
}

/// Signs in through the stand-in, which must start a session.
pub async fn signed_in(api: &Api) -> SignedIn {
    let response = sign_in(api, &grant()).await;
    assert_eq!(response.status, 201, "{}", response.json());
    SignedIn::from(&response)
}

/// Links the account that the stand-in gives for `body`'s code to `user`'s account.
pub async fn link(api: &Api, user: &SignedIn, body: &Value) -> Response {
    api.post("/v1/users/me/identities/provider", body)
        .bearer(&user.access)
        .send()
        .await
}

/// Re-authenticates `user` through the stand-in, with `extra` in the body.
pub async fn reauthenticate(api: &Api, user: &SignedIn, extra: Value) -> Response {
    api.post(
        "/v1/users/me/sessions/current/reauthenticate/provider",
        &with_fields(grant(), extra),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// Whether the request whose answer `response` is logged that it ignored a token's deletion
/// that failed, with `cause` in what it wrote of it (ERR-11).
pub fn kept_token_logged(response: &Response, cause: &str) -> bool {
    testing::logged(response.trace_id()).iter().any(|line| {
        line["error.type"] == "provider_token_not_deleted"
            && line["exception.message"]
                .as_str()
                .is_some_and(|message| message.contains(cause))
    })
}
