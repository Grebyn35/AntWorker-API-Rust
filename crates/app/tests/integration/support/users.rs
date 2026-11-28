//! Builders of users and sessions, through the API's own routes (TEST-09).

use serde_json::{Value, json};
use sqlx::PgConnection;

use platform::telemetry::testing::collector::Collector;

use super::{
    Api, Options, TestDatabase,
    api::{Call, Response},
    execute_on, queries_wait_on_locks,
};

/// A password that AUTH-003 allows.
pub const PASSWORD: &str = "plum-tractor-41-quiet";

/// The web client's origin, which [`browsers_api`] lists.
pub const WEB: &str = "https://app.example.com";

/// A registered user and their first session, with its tokens in the body.
#[derive(Clone)]
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
    registered(api, &registration(email)).await
}

/// The user and the session of a registration with `body`, such as one that [`registration`]
/// builds, with a field that a test adds, whose account must still prove its address.
pub async fn registered(api: &Api, body: &Value) -> SignedIn {
    let response = api.post("/v1/users", body).send().await;
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

/// Deletes the password identity of the account of `user_id`, through the `owner` connection, so
/// it holds no password, as an account that signs in through a provider holds none, while its
/// sessions stay open.
pub async fn without_its_password(db: &TestDatabase, user_id: &str) {
    db.owner_executes(&format!(
        "DELETE FROM identities WHERE user_id = '{user_id}'"
    ))
    .await;
}

/// Creates 1,001 accounts, `user1@example.com` and on, that hold no password, each with a
/// session that lasts, through the `owner` connection, in one statement, so a test has more of
/// them than a batch of the code takes; returns their IDs.
pub async fn many_with_a_session(db: &TestDatabase) -> Vec<String> {
    db.owner_reads(
        "WITH added AS (INSERT INTO users (email, name) \
         SELECT 'user' || n || '@example.com', 'User ' || n FROM generate_series(1, 1001) AS n \
         RETURNING id), \
         started AS (INSERT INTO sessions (user_id, access_token_hash, refresh_token_hash, \
         access_token_expires_at, refresh_token_expires_at, expires_at) \
         SELECT id, sha256(('access ' || id)::bytea), sha256(('refresh ' || id)::bytea), \
         now() + interval '1 hour', now() + interval '1 day', now() + interval '2 days' \
         FROM added) \
         SELECT id::text FROM added",
    )
    .await
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

/// The IDs of the sessions that exist, in order.
pub async fn sessions(api: &Api) -> Vec<String> {
    super::api::reads(api, "SELECT id::text FROM sessions ORDER BY 1").await
}

/// Deletes `user`'s account.
pub async fn delete(api: &Api, user: &SignedIn) -> Response {
    api.request(http::Method::DELETE, "/v1/users/me")
        .bearer(&user.access)
        .send()
        .await
}

/// The deletion of `user`'s account, as [`delete`] sends it, in a task of its own, which a test
/// holds at a lock.
pub fn deleting(api: &Api, user: &SignedIn) -> tokio::task::JoinHandle<Response> {
    let (api, user) = (api.clone(), user.clone());
    tokio::spawn(async move { delete(&api, &user).await })
}

/// Reads the current session with the access token `access`, as AUTH-016 reads it.
pub async fn current(api: &Api, access: &str) -> Response {
    api.get("/v1/users/me/sessions/current")
        .bearer(access)
        .send()
        .await
}

/// The times of a session that AUTH-016 and AUTH-043 show, by the names of their fields, which
/// are those of their columns, in the order that [`session_times`] gives them.
pub const SESSION_TIMES: [&str; 5] = [
    "created_at",
    "access_token_expires_at",
    "refresh_token_expires_at",
    "expires_at",
    "authenticated_at",
];

/// The times of the session `id` that [`SESSION_TIMES`] names, in its order, from its row, each
/// written as the API writes a time.
pub async fn session_times(api: &Api, id: &str) -> Vec<String> {
    let columns: Vec<String> = SESSION_TIMES
        .iter()
        .map(|column| {
            format!("to_char({column} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')")
        })
        .collect();
    api.database()
        .owner_reads(&format!(
            "SELECT concat_ws(' ', {}) FROM sessions WHERE id = '{id}'",
            columns.join(", ")
        ))
        .await
        .remove(0)
        .split(' ')
        .map(str::to_owned)
        .collect()
}

/// Whether `user`'s access token still authenticates a request: whether `GET /v1/users/me`
/// answers 200 with it, where it asserts that the answer is 200 or 401.
pub async fn authenticates(api: &Api, user: &SignedIn) -> bool {
    let response = api.get("/v1/users/me").bearer(&user.access).send().await;
    assert!(
        matches!(response.status.as_u16(), 200 | 401),
        "{}",
        response.status
    );
    response.status == 200
}

/// The sign-out of the session that authenticates it, `DELETE /v1/users/me/sessions/current`
/// (AUTH-013), for the test to authenticate, with an access token or a browser's cookies, and
/// send.
pub fn sign_out(api: &Api) -> Call<'_> {
    api.request(http::Method::DELETE, "/v1/users/me/sessions/current")
}

/// The record's `ETag` that `user` reads.
pub async fn etag(api: &Api, user: &SignedIn) -> String {
    api.get("/v1/users/me")
        .bearer(&user.access)
        .send()
        .await
        .header("etag")
        .to_owned()
}

/// Renames `user` to `name` with the merge patch of ACCOUNTS-007, with `if_match` if given.
pub async fn rename(api: &Api, user: &SignedIn, name: &str, if_match: Option<&str>) -> Response {
    let mut call = api
        .patch("/v1/users/me", &json!({ "name": name }))
        .bearer(&user.access);
    if let Some(etag) = if_match {
        call = call.header("if-match", etag);
    }
    call.send().await
}

/// Ends every session of `user`'s but their own.
pub async fn end_others(api: &Api, user: &SignedIn) -> Response {
    api.request(http::Method::DELETE, "/v1/users/me/sessions/others")
        .bearer(&user.access)
        .send()
        .await
}

/// Re-authenticates `user`'s session with `password`.
pub async fn reauthenticate(api: &Api, user: &SignedIn, password: &str) -> Response {
    api.post(
        "/v1/users/me/sessions/current/reauthenticate",
        &json!({ "password": password }),
    )
    .bearer(&user.access)
    .send()
    .await
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

/// Signs `email` in with `password` from the client address `peer`, with `device_token` in the
/// body when given.
pub async fn sign_in_from(
    api: &Api,
    peer: &str,
    email: &str,
    password: &str,
    device_token: Option<&str>,
) -> Response {
    let mut body = json!({ "email": email, "password": password });
    if let Some(token) = device_token {
        body["device_token"] = json!(token);
    }
    api.post("/v1/sessions", &body).from(peer).send().await
}

/// A sign-in of `email` with [`PASSWORD`] in a task of its own, which a test holds at a lock or
/// at its turn at hashing.
pub fn signing_in(api: &Api, email: &'static str) -> tokio::task::JoinHandle<Response> {
    let api = api.clone();
    tokio::spawn(async move { sign_in(&api, email, PASSWORD).await })
}

/// Changes `user`'s password from `current` to `new`, with their access token.
pub async fn change_password(api: &Api, user: &SignedIn, current: &str, new: &str) -> Response {
    api.post(
        "/v1/users/me/password/change",
        &json!({ "current_password": current, "password": new }),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// A change of `user`'s password with `body`, in a task of its own, which a test holds at a lock
/// or at its turn at hashing: its status and its body's JSON.
pub fn changing_password(
    api: &Api,
    user: &SignedIn,
    body: Value,
) -> tokio::task::JoinHandle<(http::StatusCode, Value)> {
    api.spawn(
        http::Method::POST,
        "/v1/users/me/password/change",
        &user.access,
        Some(body),
    )
}

/// Signs `email` in with [`PASSWORD`], and returns the new session.
pub async fn new_session(api: &Api, email: &str) -> SignedIn {
    let response = sign_in(api, email, PASSWORD).await;
    assert_eq!(response.status, 201, "{}", response.json());
    SignedIn::from(&response)
}

/// Refreshes the session whose refresh token is `token`, in the body.
pub async fn refresh(api: &Api, token: &str) -> Response {
    api.post("/v1/sessions/refresh", &json!({ "refresh_token": token }))
        .send()
        .await
}

/// The API with [`WEB`] among the origins of its web clients.
pub async fn browsers_api() -> Api {
    Api::with(Options::default().set("CORS_ALLOWED_ORIGINS", WEB)).await
}

/// Signs `email` in with [`PASSWORD`] from a browser at [`WEB`] that holds the cookies `cookies`,
/// a `Cookie` header that the request sends unless it is empty, and returns the response.
pub async fn sign_in_from_browser(api: &Api, email: &str, cookies: &str) -> Response {
    let call = api
        .post(
            "/v1/sessions",
            &json!({ "email": email, "password": PASSWORD }),
        )
        .header("origin", WEB);
    let call = if cookies.is_empty() {
        call
    } else {
        call.header("cookie", cookies)
    };
    call.send().await
}

/// What two sign-ins on the device of the session `session`, each a path and its body, answer
/// once both are in their units of work at once: the test holds that session's row locked until
/// two queries wait on a lock, the sign-in that holds the user's turn for the row, which it
/// deletes to end the session, and the other for the turn, or, where the two take no turn, each
/// for the row (AUTH-008).
pub async fn at_once(
    api: &Api,
    session: &str,
    sign_ins: [(&'static str, Value); 2],
) -> [Response; 2] {
    let db = api.database();
    let mut held = db
        .holding(&format!(
            "SELECT 1 FROM sessions WHERE id = '{session}' FOR UPDATE"
        ))
        .await;
    let [first, second] = sign_ins.map(|(path, body)| {
        let api = api.clone();
        tokio::spawn(async move { api.post(path, &body).send().await })
    });
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut held, "COMMIT").await;
    [first.await.unwrap(), second.await.unwrap()]
}

/// Asserts that each of `answers` started a session, and that the device `laptop` holds one
/// session, one of those that they started (AUTH-008).
pub async fn one_session_on_the_laptop(api: &Api, answers: &[Response; 2]) {
    for answer in answers {
        assert_eq!(answer.status, 201, "{}", answer.json());
    }
    let started = answers.each_ref().map(|answer| answer.json()["id"].clone());
    let on_device = api
        .database()
        .owner_reads("SELECT id::text FROM sessions WHERE device_id = 'laptop'")
        .await;
    assert_eq!(on_device.len(), 1, "{on_device:?}");
    assert!(started.contains(&json!(on_device[0])), "{on_device:?}");
}

/// Registers ada@example.com, then signs her in from a browser at [`WEB`], which takes the
/// session's tokens as cookies, and returns the sign-in's response.
pub async fn browser_sign_in(api: &Api) -> Response {
    register(api, "ada@example.com").await;
    sign_in_from_browser(api, "ada@example.com", "").await
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

/// Makes `user`'s session have proved their password `minutes` minutes ago, as
/// [`authenticated_seconds_ago`] does.
pub async fn authenticated_ago(api: &Api, user: &SignedIn, minutes: u32) {
    authenticated_seconds_ago(api, user, minutes * 60).await;
}

/// Makes `user`'s session have proved their password `seconds` seconds ago, by the database's
/// clock as it updates the row, a time in the past that only a test sets (TEST-09), as a session
/// that hasn't re-authenticated since (AUTH-023).
pub async fn authenticated_seconds_ago(api: &Api, user: &SignedIn, seconds: u32) {
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET authenticated_at = now() - make_interval(secs => {seconds}) \
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

/// What the window of `user`'s bucket named `name`, such as `password_check`, has counted, or
/// none when it has counted no request: the bucket whose key is the HMAC of `name` and the user's
/// ID under the tests' `HMAC_KEY`, as a module keys a bucket of a user's (PLATFORM-057).
pub async fn counted_in(api: &Api, name: &str, user: &SignedIn) -> Option<u32> {
    let id = platform::types::read_id(&user.user_id).unwrap();
    let key = super::hex(&super::api::hmac_key().hash(name, id.as_bytes()));
    api.database()
        .owner_reads(&format!(
            "SELECT count::text FROM rate_limit_buckets WHERE key_hash = decode('{key}', 'hex')"
        ))
        .await
        .first()
        .map(|count| count.parse().unwrap())
}

/// The ID of the one bucket that `request`, sent once, adds to those there before it.
pub async fn added_bucket(
    api: &Api,
    request: impl std::future::Future<Output = Response>,
) -> String {
    added_bucket_of(api, request).await.0
}

/// The ID of the one bucket that `request`, sent once, adds to those there before it, and what
/// the request answered, such as a proof's answer, which holds its session's new tokens
/// (AUTH-095).
pub async fn added_bucket_of(
    api: &Api,
    request: impl std::future::Future<Output = Response>,
) -> (String, Response) {
    let before = buckets(api).await;
    let answer = request.await;
    let mut added: Vec<String> = buckets(api)
        .await
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(added.len(), 1, "{added:?}");
    (added.remove(0), answer)
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

/// The statement that locks the rows of `user`'s identities, as a sign-in of theirs locks their
/// password identity.
pub fn identities_lock(user: &SignedIn) -> String {
    format!(
        "SELECT 1 FROM identities WHERE user_id = '{}' FOR UPDATE",
        user.user_id
    )
}

/// A connection as `owner` that holds the rows of `user`'s identities locked, as
/// [`identities_lock`] locks them, in a transaction that the test ends with [`execute_on`], so
/// that what locks one of the rows meanwhile waits for it.
pub async fn holding_identities(db: &TestDatabase, user: &SignedIn) -> PgConnection {
    db.holding(&identities_lock(user)).await
}

/// How many passwords the request of each of `traces` verified, by the `verify_password` spans of
/// its trace, once `collector` holds each request's own span, which ends after the spans inside
/// it, so its export follows theirs.
pub async fn password_verifications(collector: &Collector, traces: &[&str]) -> Vec<usize> {
    let received = collector
        .wait_for(|received| {
            traces.iter().all(|trace| {
                received
                    .spans()
                    .iter()
                    .any(|(_, span)| super::is_the_request_s(span, trace))
            })
        })
        .await;
    traces
        .iter()
        .map(|trace| {
            received
                .spans()
                .into_iter()
                .filter(|(_, span)| {
                    super::hex(&span.trace_id) == *trace && span.name == "verify_password"
                })
                .count()
        })
        .collect()
}

/// What a check of a signed-in user's password, a `POST` to `path` with the body that `body` makes
/// of a password, answers on a server that exports its spans, with email off: for a wrong
/// password of an account that has one, then for [`PASSWORD`] of an account whose password
/// identity is gone, as one that signs in through a provider has none, with its session still
/// open, each answer's status, its `code`, and how many passwords it verified, as
/// [`password_verifications`] counts them (AUTH-021, AUTH-040).
pub async fn checks_without_a_password(
    path: &str,
    body: fn(&str) -> Value,
) -> Vec<(u16, Value, usize)> {
    let collector = Collector::start().await.unwrap();
    let database = TestDatabase::migrated().await;
    let server = database.exporting_to(&collector, &[("EMAIL", "off")]).await;
    let mut sessions = Vec::new();
    for email in ["ada@example.com", "bob@example.com"] {
        let registration = registration(email).to_string();
        let (status, registered) = server
            .send(
                "POST",
                "/v1/users",
                &[("content-type", "application/json")],
                registration.as_bytes(),
            )
            .await;
        assert_eq!(status, 201, "{registered}");
        sessions.push((
            text(&registered["user_id"]),
            text(&registered["access_token"]),
        ));
    }
    without_its_password(&database, &sessions[1].0).await;
    let traces = [
        "5bf1a8b1a1e04e7a8c0f1d2e3a4b5c21",
        "5bf1a8b1a1e04e7a8c0f1d2e3a4b5c22",
    ];
    let mut answers = Vec::new();
    for ((_, access), (password, trace)) in sessions
        .iter()
        .zip([("wrong-password-123", traces[0]), (PASSWORD, traces[1])])
    {
        let bearer = format!("Bearer {access}");
        let parent = format!("00-{trace}-b7ad6b7169203331-01");
        let sent = body(password).to_string();
        let (status, answer) = server
            .send(
                "POST",
                path,
                &[
                    ("authorization", &bearer),
                    ("traceparent", &parent),
                    ("content-type", "application/json"),
                ],
                sent.as_bytes(),
            )
            .await;
        answers.push((status, answer["code"].clone()));
    }
    let verified = password_verifications(&collector, &traces).await;
    answers
        .into_iter()
        .zip(verified)
        .map(|((status, code), verified)| (status, code, verified))
        .collect()
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
