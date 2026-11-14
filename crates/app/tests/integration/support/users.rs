//! Builders of users and sessions, through the API's own routes (TEST-09).

use serde_json::{Value, json};

use super::{Api, Options, TestDatabase, api::Response, execute_on, queries_wait_on_locks};

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

/// Registers `email`, and returns the user and their session, whose account has proved its
/// address, as one that confirmed its first code has (AUTH-031), so a test of anything else needs
/// no code: see [`verified`]. The registration publishes `session.created` (AUTH-046), so a test
/// that reads the whole `events` table, or a stream's newest event, deletes the setup's events
/// first.
pub async fn register(api: &Api, email: &str) -> SignedIn {
    let user = register_unverified(api, email).await;
    verified(api, &user.user_id).await;
    user
}

/// Registers `email`, and returns the user and their session, whose account must still prove its
/// address while email is on (AUTH-025).
pub async fn register_unverified(api: &Api, email: &str) -> SignedIn {
    let response = api.post("/v1/users", &registration(email)).send().await;
    assert_eq!(response.status, 201, "{}", response.json());
    SignedIn::from(&response)
}

/// Makes the account of `user_id` one that proved its address, through the `owner` connection:
/// its `email_verified` is true, and the job that would send its first code is gone, as once that
/// job has run and its code was confirmed (AUTH-030, AUTH-031).
pub async fn verified(api: &Api, user_id: &str) {
    api.database()
        .owner_executes(&format!(
            "UPDATE users SET email_verified = true WHERE id = '{user_id}';
            DELETE FROM jobs WHERE kind = 'auth.send_verification_code' AND user_id = '{user_id}'"
        ))
        .await;
}

/// Creates an account of `email` that holds no password, as an account that a sign-in provider
/// creates does, through the `owner` connection, so a test without the in-process API has a user
/// too; returns its ID.
pub async fn without_password(db: &TestDatabase, email: &str) -> String {
    let email = email.replace('\'', "''");
    db.owner_reads(&format!(
        "INSERT INTO users (email, name) VALUES ('{email}', 'Ada Lovelace') RETURNING id::text"
    ))
    .await
    .remove(0)
}

// part: workspace_setup
/// Creates an account of `email` whose one identity is an account at a provider, as a sign-in
/// through a provider registers one, through the `owner` connection, with its address proved
/// when `proved` says so; returns its ID.
pub async fn with_provider_alone(db: &TestDatabase, email: &str, proved: bool) -> String {
    let id = without_password(db, email).await;
    db.owner_executes(&format!(
        "UPDATE users SET email_verified = {proved} WHERE id = '{id}'; \
         INSERT INTO provider_identities (user_id, provider, subject) \
         VALUES ('{id}', 'stand_in', 'subject-of-{id}')"
    ))
    .await;
    id
}
// end part: workspace_setup

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

/// The cookie `__Host-<name>` that `response` sets, whole.
fn set_cookie<'a>(response: &'a Response, name: &str) -> &'a str {
    let prefix = format!("__Host-{name}=");
    let set = response
        .headers("set-cookie")
        .into_iter()
        .find(|cookie| cookie.starts_with(&prefix));
    assert!(set.is_some(), "no {name} cookie");
    set.unwrap()
}

/// The name, after `__Host-`, of the device token's cookie that `response` sets: the one of its
/// account's email (AUTH-015).
pub fn device_cookie(response: &Response) -> String {
    let set = response
        .headers("set-cookie")
        .into_iter()
        .find(|cookie| cookie.starts_with("__Host-device_token_"));
    assert!(set.is_some(), "no device token's cookie");
    set.unwrap()["__Host-".len()..]
        .split('=')
        .next()
        .unwrap()
        .to_owned()
}

/// The value of the cookie `__Host-<name>` that `response` sets.
pub fn cookie(response: &Response, name: &str) -> String {
    let set = set_cookie(response, name);
    set[set.find('=').unwrap() + 1..]
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// The `Expires` of the cookie `__Host-<name>` that `response` sets.
pub fn expires(response: &Response, name: &str) -> String {
    let set = set_cookie(response, name);
    let expires = set
        .split("; ")
        .find_map(|attribute| attribute.strip_prefix("Expires="));
    assert!(expires.is_some(), "{set}");
    expires.unwrap().to_owned()
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

/// Makes `user`'s session have proved their password `minutes` minutes ago, a time in the past
/// that only a test sets (TEST-09), as a session that hasn't re-authenticated since (AUTH-023).
pub async fn authenticated_ago(api: &Api, user: &SignedIn, minutes: u32) {
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET authenticated_at = now() - make_interval(mins => {minutes}) \
             WHERE id = '{}'",
            user.session_id
        ))
        .await;
}

/// Each event named `name`, `session.created` or `session.ended`, as its user's ID and the ID of
/// the session its payload names, sorted (AUTH-046).
pub async fn session_events(api: &Api, name: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT user_id || ' ' || (payload->>'session_id') FROM events \
             WHERE name = '{name}' ORDER BY 1"
        ))
        .await
}

/// The IDs of every rate-limit bucket, sorted, so a test finds the bucket a request adds by the
/// IDs before and after it, since a bucket's key is a keyed hash it can't name (PLATFORM-057).
pub async fn buckets(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads("SELECT id::text FROM rate_limit_buckets ORDER BY 1")
        .await
}

/// The ID of the one bucket that `request`, sent once, adds to those there before it.
pub async fn added_bucket(
    api: &Api,
    request: impl std::future::Future<Output = Response>,
) -> String {
    let before = buckets(api).await;
    request.await;
    let mut added: Vec<String> = buckets(api)
        .await
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(added.len(), 1, "{added:?}");
    added.remove(0)
}

/// Makes `user`'s session one that stopped lasting an hour ago: its tokens and its absolute
/// lifetime expired, a time in the past that only a test sets (TEST-09).
pub async fn stopped_lasting(api: &Api, user: &SignedIn) {
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET access_token_expires_at = now() - interval '1 hour', \
             refresh_token_expires_at = now() - interval '1 hour', \
             expires_at = now() - interval '1 hour' WHERE id = '{}'",
            user.session_id
        ))
        .await;
}

/// What `user`'s request answers, a `request.0` of `request.1` with `body` when it has one, when
/// another request ends its session once the request is authenticated: it waits at the bucket
/// `bucket`, which it counts once its session is verified, while the session ends (AUTH-016).
pub async fn after_its_session_ended(
    api: &Api,
    user: &SignedIn,
    bucket: &str,
    request: (http::Method, &str),
    body: Option<Value>,
) -> (http::StatusCode, Value) {
    let db = api.database();
    let mut ender = db
        .holding(&format!(
            "SELECT 1 FROM rate_limit_buckets WHERE id = '{bucket}' FOR UPDATE"
        ))
        .await;
    let (method, path) = request;
    let sent = api.spawn(method, path, &user.access, body);
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut ender,
        &format!("DELETE FROM sessions WHERE id = '{}'", user.session_id),
    )
    .await;
    execute_on(&mut ender, "COMMIT").await;
    sent.await.unwrap()
}
