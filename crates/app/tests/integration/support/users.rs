//! Builders of users and sessions, through the API's own routes (TEST-10).

use serde_json::{Value, json};

use super::{Api, Options, api::Response};

/// A password that AUTH-003 allows.
pub const PASSWORD: &str = "plum-tractor-41-quiet";

/// The web client's origin, which [`browsers_api`] lists.
pub const WEB: &str = "https://app.example.com";

/// A registered user and their first session, with its tokens in the body.
pub struct SignedIn {
    pub user_id: String,
    pub session_id: String,
    pub access: String,
    pub refresh: String,
}

impl SignedIn {
    /// The session in `response`'s body.
    pub fn from(response: &Response) -> Self {
        let body = response.json();
        Self {
            user_id: text(&body["user_id"]),
            session_id: text(&body["id"]),
            access: text(&body["access_token"]),
            refresh: text(&body["refresh_token"]),
        }
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap().to_owned()
}

/// The body of a registration of `email`, named Ada, with [`PASSWORD`].
pub fn registration(email: &str) -> Value {
    json!({ "email": email, "password": PASSWORD, "name": "Ada Lovelace" })
}

/// Registers `email`, and returns the user and their session.
pub async fn register(api: &Api, email: &str) -> SignedIn {
    let response = api.post("/v1/users", &registration(email)).send().await;
    assert_eq!(response.status, 201, "{}", response.json());
    SignedIn::from(&response)
}

/// Signs `email` in with `password`, and returns the response.
pub async fn sign_in(api: &Api, email: &str, password: &str) -> Response {
    api.post(
        "/v1/sessions",
        &json!({ "email": email, "password": password }),
    )
    .send()
    .await
}

/// Signs `email` in with [`PASSWORD`], and returns the new session.
pub async fn new_session(api: &Api, email: &str) -> SignedIn {
    let response = sign_in(api, email, PASSWORD).await;
    assert_eq!(response.status, 201, "{}", response.json());
    SignedIn::from(&response)
}

/// The API with [`WEB`] among the origins of its web clients.
pub async fn browsers_api() -> Api {
    Api::with(Options::default().set("CORS_ALLOWED_ORIGINS", WEB)).await
}

/// Registers ada@example.com, then signs her in from a browser at [`WEB`], which takes the
/// session's tokens as cookies, and returns the sign-in's response.
pub async fn browser_sign_in(api: &Api) -> Response {
    register(api, "ada@example.com").await;
    api.post(
        "/v1/sessions",
        &json!({ "email": "ada@example.com", "password": PASSWORD }),
    )
    .header("origin", WEB)
    .send()
    .await
}

/// The value of the cookie `__Host-<name>` that `response` sets.
pub fn cookie(response: &Response, name: &str) -> String {
    let prefix = format!("__Host-{name}=");
    let set = response
        .headers("set-cookie")
        .into_iter()
        .find(|cookie| cookie.starts_with(&prefix));
    assert!(set.is_some(), "no {name} cookie");
    set.unwrap()[prefix.len()..]
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// A `Cookie` header that sends back every cookie `response` sets.
pub fn cookies(response: &Response) -> String {
    response
        .headers("set-cookie")
        .iter()
        .map(|cookie| cookie.split(';').next().unwrap())
        .collect::<Vec<_>>()
        .join("; ")
}
