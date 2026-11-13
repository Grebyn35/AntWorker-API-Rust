//! The turn at hashing that a sign-in, and a check of a signed-in user's password, take before
//! their unit of work (AUTH-093).
//!
//! A test takes turns of the API's workers to arrange how many are free, and holds a lock to
//! keep a request where it stands; what it asserts is each request's answer. A request that
//! waited for a turn that never came would answer 503 at its route's timeout.

use argon2::{Algorithm, Argon2, Params, PasswordHasher, Version};
use platform::blocking::Turn;
use serde_json::json;

use crate::support::{
    Api, TestDatabase,
    api::Response,
    execute_on, queries_wait_on_locks,
    second_factors::{add_app, code},
    users::{self, PASSWORD},
};

const NEW_PASSWORD: &str = "walnut-ferry-88-bright";

/// Takes every turn of the API's CPU-heavy work, as a burst of sign-ins does.
async fn every_turn(api: &Api) -> Vec<Turn> {
    let workers = api.workers();
    let mut turns = Vec::new();
    for _ in 0..workers.turns() {
        turns.push(workers.turn().await.unwrap());
    }
    turns
}

/// Takes every turn but one, which the test's requests hash in, one at once.
async fn every_turn_but_one(api: &Api) -> Vec<Turn> {
    let mut turns = every_turn(api).await;
    turns.pop();
    turns
}

/// A sign-in in a task of its own, which a test holds at a lock or at its turn.
fn signing_in(api: &Api, email: &'static str) -> tokio::task::JoinHandle<Response> {
    let api = api.clone();
    tokio::spawn(async move { users::sign_in(&api, email, PASSWORD).await })
}

/// A change of `user`'s password in a task of its own, with `body`.
fn changing(
    api: &Api,
    user: &users::SignedIn,
    body: serde_json::Value,
) -> tokio::task::JoinHandle<(http::StatusCode, serde_json::Value)> {
    api.spawn(
        http::Method::POST,
        "/v1/users/me/password/change",
        &user.access,
        Some(body),
    )
}

/// A sign-in that no account answers to, which hashes all the same (AUTH-005): its answer
/// shows that a turn was free for it.
async fn another_request_hashes(api: &Api) {
    let other = users::sign_in(api, "nobody@example.com", PASSWORD).await;
    assert_eq!(other.status, 401, "{}", other.json());
}

/// Waits until `count` callers wait for a turn at hashing.
async fn wait_for_turns(api: &Api, count: usize) {
    while api.workers().waiting() < count {
        tokio::task::yield_now().await;
    }
}

/// The statement that holds the row of `user`'s password identity, at which their sign-in waits
/// once it has its turn at hashing, and so keeps that turn.
fn identity_row(user: &users::SignedIn) -> String {
    format!(
        "SELECT 1 FROM identities WHERE user_id = '{}' FOR UPDATE",
        user.user_id
    )
}

#[tokio::test]
async fn auth_093_sign_ins_that_wait_for_their_turn_at_hashing_hold_no_connection_of_the_pool() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let turns = every_turn(&api).await;
    // More sign-ins than the pool has connections, each of which waits for a turn.
    let waiting: Vec<_> = (0..usize::try_from(TestDatabase::POOL_SIZE).unwrap() + 2)
        .map(|_| signing_in(&api, "ada@example.com"))
        .collect();
    wait_for_turns(&api, waiting.len()).await;

    // Other requests, each of which needs a connection of the pool, are served meanwhile.
    let capabilities = api.get("/v1/capabilities").send().await;
    let record = api.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(capabilities.status, 200);
    assert_eq!(record.status, 200);
    drop(turns);
    for sign_in in waiting {
        assert_eq!(sign_in.await.unwrap().status, 201);
    }
}

#[tokio::test]
async fn auth_093_checks_of_a_signed_in_user_s_password_wait_for_their_turn_the_same_way() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let turns = every_turn(&api).await;
    // More checks than the pool has connections, and as many as their bucket takes (AUTH-022).
    let waiting: Vec<_> = (0..usize::try_from(TestDatabase::POOL_SIZE).unwrap() + 1)
        .map(|_| {
            api.spawn(
                http::Method::POST,
                "/v1/users/me/sessions/current/reauthenticate",
                &ada.access,
                Some(json!({ "password": PASSWORD })),
            )
        })
        .collect();
    wait_for_turns(&api, waiting.len()).await;

    let record = api.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(record.status, 200);
    drop(turns);
    for check in waiting {
        assert_eq!(check.await.unwrap().0, 200);
    }
}

#[tokio::test]
async fn auth_093_a_sign_in_s_rehash_is_made_in_the_turn_it_took_first() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    // Ada's password is stored with other parameters, so her sign-in hashes it again.
    let older = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(8_192, 3, 1, None).unwrap(),
    )
    .hash_password_with_salt(PASSWORD.as_bytes(), b"saltsaltsaltsalt")
    .unwrap()
    .to_string();
    db.owner_executes(&format!(
        "UPDATE identities SET password_hash = '{older}' WHERE user_id = '{}'",
        ada.user_id
    ))
    .await;
    let mut held = db.holding(&identity_row(&bob)).await;
    let mut turns = every_turn(&api).await;
    // Ada's sign-in waits for a turn, and Bob's behind it.
    let ada_signing_in = signing_in(&api, "ada@example.com");
    wait_for_turns(&api, 1).await;
    let bob_signing_in = signing_in(&api, "bob@example.com");
    wait_for_turns(&api, 2).await;

    // One turn comes free. Ada's sign-in takes it, and Bob's takes it next, and keeps it at his
    // identity's row: a rehash that waited for a turn of its own would wait behind Bob's.
    turns.pop();
    let signed_in = ada_signing_in.await.unwrap();

    assert_eq!(signed_in.status, 201, "{}", signed_in.json());
    let stored = db
        .owner_reads(&format!(
            "SELECT password_hash FROM identities WHERE user_id = '{}'",
            ada.user_id
        ))
        .await;
    assert!(stored[0].starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    execute_on(&mut held, "COMMIT").await;
    assert_eq!(bob_signing_in.await.unwrap().status, 201);
}

#[tokio::test]
async fn auth_093_a_change_s_new_password_is_hashed_in_the_turn_its_check_took() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let mut held = db.holding(&identity_row(&bob)).await;
    let mut turns = every_turn(&api).await;
    // Ada's change waits for a turn, and Bob's sign-in behind it.
    let body = json!({ "current_password": PASSWORD, "password": NEW_PASSWORD });
    let ada_changing = changing(&api, &ada, body);
    wait_for_turns(&api, 1).await;
    let bob_signing_in = signing_in(&api, "bob@example.com");
    wait_for_turns(&api, 2).await;

    // One turn comes free. Ada's change takes it, and Bob's sign-in takes it next, and keeps it
    // at his identity's row: a new hash that waited for a turn of its own would wait behind it.
    turns.pop();
    let (status, body) = ada_changing.await.unwrap();

    assert_eq!(status, 204, "{body}");
    execute_on(&mut held, "COMMIT").await;
    assert_eq!(bob_signing_in.await.unwrap().status, 201);
    drop(turns);
    let with_the_new = users::sign_in(&api, "ada@example.com", NEW_PASSWORD).await;
    assert_eq!(with_the_new.status, 201);
}

#[tokio::test]
async fn auth_093_a_sign_in_gives_its_turn_up_before_its_writes() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;
    let db = api.database();
    let busy = every_turn_but_one(&api).await;
    // Held against the sign-in's first write, which ends the device's session, and against
    // none of its reads.
    let mut held = db
        .holding("LOCK TABLE sessions IN SHARE ROW EXCLUSIVE MODE")
        .await;

    let ada_signing_in = signing_in(&api, "ada@example.com");
    queries_wait_on_locks(db, 1).await;

    // The sign-in has verified the password, and waits to write: the one turn is free.
    another_request_hashes(&api).await;
    execute_on(&mut held, "COMMIT").await;
    assert_eq!(ada_signing_in.await.unwrap().status, 201);
    drop(busy);
}

#[tokio::test]
async fn auth_093_a_change_of_the_password_gives_its_turn_up_before_its_writes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let busy = every_turn_but_one(&api).await;
    // Held against the change's first write, which sets the password's hash, and against none
    // of its reads.
    let mut held = db
        .holding("LOCK TABLE identities IN SHARE ROW EXCLUSIVE MODE")
        .await;

    let body = json!({ "current_password": PASSWORD, "password": NEW_PASSWORD });
    let ada_changing = changing(&api, &ada, body);
    queries_wait_on_locks(db, 1).await;

    // The change has verified the current password and hashed the new one, and waits to write.
    another_request_hashes(&api).await;
    execute_on(&mut held, "COMMIT").await;
    assert_eq!(ada_changing.await.unwrap().0, 204);
    drop(busy);
}

#[tokio::test]
async fn auth_093_a_change_with_a_wrong_current_password_gives_its_turn_up_before_its_record() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let busy = every_turn_but_one(&api).await;
    // Held against the record of the wrong password, the change's only write.
    let mut held = db
        .holding("LOCK TABLE audit_entries IN SHARE ROW EXCLUSIVE MODE")
        .await;

    let body = json!({ "current_password": "not-her-password-12", "password": NEW_PASSWORD });
    let ada_changing = changing(&api, &ada, body);
    queries_wait_on_locks(db, 1).await;

    // The change has verified the password, and waits to record that it was wrong.
    another_request_hashes(&api).await;
    execute_on(&mut held, "COMMIT").await;
    let (status, body) = ada_changing.await.unwrap();
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["code"], "auth.wrong_password");
    drop(busy);
}

#[tokio::test]
async fn auth_093_a_change_by_an_account_with_an_app_gives_its_turn_up_before_the_code_s_step() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let app = add_app(&api, &ada).await;
    let db = api.database();
    let code = code(&api, &app.secret).await;
    let busy = every_turn_but_one(&api).await;
    // Held against the record of the step that the code's check accepts, the change's first
    // write, and against none of its reads.
    let mut held = db
        .holding("LOCK TABLE totp_factors IN SHARE ROW EXCLUSIVE MODE")
        .await;

    let body = json!({ "current_password": PASSWORD, "password": NEW_PASSWORD, "code": code });
    let ada_changing = changing(&api, &ada, body);
    queries_wait_on_locks(db, 1).await;

    // The change has verified the current password and hashed the new one, and waits to record
    // the code's step.
    another_request_hashes(&api).await;
    execute_on(&mut held, "COMMIT").await;
    let (status, body) = ada_changing.await.unwrap();
    assert_eq!(status, 204, "{body}");
    drop(busy);
}
