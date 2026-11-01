//! Builders of users and sessions, through the API's own routes (TEST-10).

use serde_json::{Value, json};

use super::{Api, api::Response};

/// A password that AUTH-003 allows.
pub const PASSWORD: &str = "plum-tractor-41-quiet";

/// A registered user and their first session, with its tokens in the body.
pub struct SignedIn {
    pub user_id: String,
    pub session_id: String,
    pub access: String,
    pub refresh: String,
}

impl SignedIn {
    fn from(response: &Response) -> Self {
        let body = response.json();
        Self {
            user_id: text(&body["user"]["id"]),
            session_id: text(&body["session"]["id"]),
            access: text(&body["session"]["access_token"]),
            refresh: text(&body["session"]["refresh_token"]),
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
