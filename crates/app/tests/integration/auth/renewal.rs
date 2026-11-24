//! The new tokens that each proof of who a session's user is gives the session (the auth spec's
//! "Re-authentication", AUTH-095).

use serde_json::json;

use crate::support::{
    Api,
    api::Response,
    api::reads,
    execute_on,
    providers::{self, gives, grant, identity, stand_in},
    queries_wait_on_locks,
    second_factors::{self, add_app, other_code},
    upstream::Upstream,
    users::{self, PASSWORD, SignedIn, WEB},
};

/// Asserts that `response` answers `user`'s session with a new pair of tokens, that the pair it
/// had stops working, and that the new pair works; returns the session with the new pair.
async fn renewed(api: &Api, user: &SignedIn, response: &Response) -> SignedIn {
    assert_eq!(response.status, 200, "{}", response.json());
    let after = SignedIn::from(response);
    assert_eq!(after.session_id, user.session_id);
    assert_eq!(after.user_id, user.user_id);
    assert_ne!(after.access, user.access);
    assert_ne!(after.refresh, user.refresh);
    assert_eq!(users::current(api, &user.access).await.status, 401);
    assert_eq!(users::current(api, &after.access).await.status, 200);
    after
}

#[tokio::test]
async fn auth_095_a_reauthentication_gives_the_session_new_tokens_as_a_refresh_does() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // A session whose refresh token would expire in a day, and which ends in two.
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET refresh_token_expires_at = now() + interval '1 day', \
             expires_at = now() + interval '2 days' WHERE id = '{}'",
            ada.session_id
        ))
        .await;

    let response = users::reauthenticate(&api, &ada, PASSWORD).await;

    let after = renewed(&api, &ada, &response).await;
    // The refresh expiry slides, but not past the absolute lifetime, which stays.
    assert_eq!(
        reads(
            &api,
            "SELECT (refresh_token_expires_at = expires_at \
             AND expires_at < now() + interval '2 days' \
             AND access_token_expires_at > now() + interval '59 minutes')::text FROM sessions"
        )
        .await,
        ["true"]
    );
    // The new refresh token refreshes the session.
    let refreshed = users::refresh(&api, &after.refresh).await;
    assert_eq!(refreshed.status, 200, "{}", refreshed.json());
}

#[tokio::test]
async fn auth_095_the_refresh_token_that_a_proof_replaced_ends_the_session_when_presented() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let response = users::reauthenticate(&api, &ada, PASSWORD).await;
    let after = renewed(&api, &ada, &response).await;

    let reused = users::refresh(&api, &ada.refresh).await;

    assert_eq!(reused.status, 401, "{}", reused.json());
    assert_eq!(reused.json()["code"], "auth.invalid_refresh_token");
    assert_eq!(users::current(&api, &after.access).await.status, 401);
    assert_eq!(
        reads(
            &api,
            "SELECT resource_id::text FROM audit_entries \
             WHERE action = 'session.reuse_detected'"
        )
        .await,
        std::slice::from_ref(&ada.session_id)
    );
}

#[tokio::test]
async fn auth_095_a_change_of_the_password_gives_the_caller_s_session_new_tokens() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = users::change_password(&api, &ada, PASSWORD, "walnut-harbor-77-brisk").await;

    let after = renewed(&api, &ada, &response).await;
    let session = response.json();
    assert_eq!(session["email_verification_required"], false);
    assert_eq!(session["second_factor_setup_required"], false);
    let stored = reads(
        &api,
        "SELECT to_char(authenticated_at AT TIME ZONE 'UTC', \
         'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM sessions",
    )
    .await;
    assert_eq!(session["authenticated_at"], stored[0].as_str());
    // The pair that the change replaced, presented, ends the session, as after a refresh.
    let reused = users::refresh(&api, &ada.refresh).await;
    assert_eq!(reused.status, 401, "{}", reused.json());
    assert_eq!(users::current(&api, &after.access).await.status, 401);
}

#[tokio::test]
async fn auth_095_a_reauthentication_through_a_provider_gives_the_session_new_tokens() {
    let upstream = Upstream::start().await;
    let api = Api::with(stand_in(&upstream)).await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = providers::signed_in(&api).await;

    let response = api
        .post(
            "/v1/users/me/sessions/current/reauthenticate/provider",
            &grant(),
        )
        .bearer(&ada.access)
        .send()
        .await;

    renewed(&api, &ada, &response).await;
}

#[tokio::test]
async fn auth_095_a_browser_takes_the_new_tokens_as_cookies() {
    let api = users::browsers_api().await;
    let signed_in = users::browser_sign_in(&api).await;
    assert_eq!(signed_in.status, 201, "{}", signed_in.json());
    let before = users::cookie(&signed_in, "access_token");

    let response = api
        .post(
            "/v1/users/me/sessions/current/reauthenticate",
            &json!({ "password": PASSWORD }),
        )
        .header("origin", WEB)
        .header("cookie", &users::cookies(&signed_in))
        .send()
        .await;

    assert_eq!(response.status, 200, "{}", response.json());
    let session = response.json();
    assert!(session.get("access_token").is_none(), "{session}");
    assert!(session.get("refresh_token").is_none(), "{session}");
    let access = users::cookie(&response, "access_token");
    assert_ne!(access, before);
    assert_ne!(
        users::cookie(&response, "refresh_token"),
        users::cookie(&signed_in, "refresh_token")
    );
    assert_eq!(users::current(&api, &before).await.status, 401);
    assert_eq!(users::current(&api, &access).await.status, 200);
}

#[tokio::test]
async fn auth_095_of_two_proofs_sent_at_once_the_pair_that_the_second_answers_holds() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    // Another request holds the user's turn, which each proof waits for.
    let mut held = db
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR NO KEY UPDATE",
            ada.user_id
        ))
        .await;
    let proof = || {
        api.spawn(
            http::Method::POST,
            "/v1/users/me/sessions/current/reauthenticate",
            &ada.access,
            Some(json!({ "password": PASSWORD })),
        )
    };

    // The second is sent once the first waits, so it waits behind it and takes its turn second.
    let first = proof();
    queries_wait_on_locks(db, 1).await;
    let second = proof();
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut held, "COMMIT").await;
    let (first, second) = (first.await.unwrap(), second.await.unwrap());

    assert_eq!(first.0, 200, "{}", first.1);
    assert_eq!(second.0, 200, "{}", second.1);
    let access = |answer: &serde_json::Value| answer["access_token"].as_str().unwrap().to_owned();
    assert_eq!(users::current(&api, &access(&second.1)).await.status, 200);
    assert_eq!(users::current(&api, &access(&first.1)).await.status, 401);
}

#[tokio::test]
async fn auth_095_a_proof_that_fails_leaves_the_session_its_tokens() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let hashes = "SELECT encode(access_token_hash, 'hex') || encode(refresh_token_hash, 'hex') \
                  || coalesce(encode(previous_refresh_token_hash, 'hex'), '-') FROM sessions";
    let before = reads(&api, hashes).await;

    let wrong = users::reauthenticate(&api, &ada, "not-the-password-at-all").await;
    let wrong_change = users::change_password(
        &api,
        &ada,
        "not-the-password-at-all",
        "walnut-harbor-77-brisk",
    )
    .await;

    assert_eq!(wrong.status, 403, "{}", wrong.json());
    assert_eq!(wrong_change.status, 403, "{}", wrong_change.json());
    assert_eq!(reads(&api, hashes).await, before);
    assert_eq!(users::current(&api, &ada.access).await.status, 200);
    // The right password with a code that isn't the app's.
    let app = add_app(&api, &ada).await;
    let code = other_code(&second_factors::code(&api, &app.secret).await);
    let wrong_code = api
        .post(
            "/v1/users/me/sessions/current/reauthenticate",
            &json!({ "password": PASSWORD, "code": code }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(wrong_code.status, 403, "{}", wrong_code.json());
    assert_eq!(wrong_code.json()["code"], "auth.wrong_second_factor");
    assert_eq!(reads(&api, hashes).await, before);
}
