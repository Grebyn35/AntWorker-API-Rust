//! Re-authenticating, and the deletion of an account that it confirms (the auth spec's
//! "Re-authentication" and "Deleting an account").

use serde_json::json;

use crate::support::{
    Api,
    api::Response,
    execute_on, query_waits_on_a_lock,
    users::{self, PASSWORD, SignedIn},
};

/// When `user`'s session last proved their password, as the database holds it.
async fn authenticated_at(api: &Api, user: &SignedIn) -> String {
    api.database()
        .owner_reads(&format!(
            "SELECT authenticated_at::text FROM sessions WHERE id = '{}'",
            user.session_id
        ))
        .await
        .remove(0)
}

#[tokio::test]
async fn auth_021_the_right_password_reauthenticates_the_session() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 10).await;
    let before = authenticated_at(&api, &ada).await;

    let response = users::reauthenticate(&api, &ada, PASSWORD).await;

    assert_eq!(response.status, 200);
    let session = response.json();
    assert_eq!(session["id"], ada.session_id.as_str());
    // The session's new tokens come with it (AUTH-095).
    assert!(session["access_token"].is_string(), "{session}");
    assert!(session["refresh_token"].is_string(), "{session}");
    assert!(session.get("device_token").is_none(), "{session}");
    let after = authenticated_at(&api, &ada).await;
    assert!(after > before, "{before} then {after}");
    let stored = api
        .database()
        .owner_reads(&format!(
            "SELECT to_char(authenticated_at AT TIME ZONE 'UTC', \
             'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM sessions WHERE id = '{}'",
            ada.session_id
        ))
        .await;
    assert_eq!(session["authenticated_at"], stored[0].as_str());
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT resource_id::text FROM audit_entries WHERE action = 'session.reauthenticated'"
            )
            .await,
        std::slice::from_ref(&ada.session_id)
    );
}

#[tokio::test]
async fn auth_021_a_wrong_password_answers_403_records_it_and_changes_nothing_else() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 10).await;
    let before = authenticated_at(&api, &ada).await;

    let response = users::reauthenticate(&api, &ada, "wrong-password-123").await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "auth.wrong_password");
    assert_eq!(authenticated_at(&api, &ada).await, before);
    let ada_id = &ada.user_id;
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT scope || ' ' || user_id || ' ' || actor_user_id FROM audit_entries \
                 WHERE action = 'password.check_failed'"
            )
            .await,
        [format!("user {ada_id} {ada_id}")]
    );
    let still = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(still.status, 200);
}

#[tokio::test]
async fn auth_021_an_account_without_a_password_answers_403_as_a_wrong_password_does() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // An account whose password identity is gone, as one that signs in through a provider has
    // none, with a session still open.
    users::without_its_password(api.database(), &ada.user_id).await;

    let response = users::reauthenticate(&api, &ada, PASSWORD).await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "auth.wrong_password");
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT count(*)::text FROM audit_entries WHERE action = 'password.check_failed'"
            )
            .await,
        ["1"]
    );
}

#[tokio::test]
async fn auth_021_an_account_without_a_password_verifies_a_hash_as_a_wrong_password_does() {
    let answers = users::checks_without_a_password(
        "/v1/users/me/sessions/current/reauthenticate",
        |password| json!({ "password": password }),
    )
    .await;

    // A wrong password's, then a missing one's.
    for (status, code, verified) in answers {
        assert_eq!(status, 403);
        assert_eq!(code, "auth.wrong_password");
        assert_eq!(verified, 1);
    }
}

#[tokio::test]
async fn auth_022_every_check_of_a_signed_in_user_s_password_counts_in_one_bucket_of_theirs() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;

    for _ in 0..5 {
        let wrong = users::reauthenticate(&api, &ada, "wrong-password-123").await;
        assert_eq!(wrong.status, 403);
    }
    let limited = users::reauthenticate(&api, &ada, PASSWORD).await;
    let other = users::reauthenticate(&api, &bob, PASSWORD).await;

    assert_eq!(limited.status, 429);
    assert_eq!(limited.json()["code"], "request.rate_limited");
    // 5 in a window of 15 minutes.
    assert!(
        limited
            .header("ratelimit-policy")
            .starts_with("\"password_check\";q=5;w=900"),
        "{}",
        limited.header("ratelimit-policy")
    );
    assert_eq!(other.status, 200);
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT count(*)::text FROM audit_entries WHERE action = 'password.check_failed'"
            )
            .await,
        ["5"]
    );
}

#[tokio::test]
async fn auth_021_a_session_that_another_request_ended_meanwhile_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    // Every bucket of Ada's requests but her password checks', which a re-authentication adds.
    api.get("/v1/users/me").bearer(&ada.access).send().await;
    let (checks, first) =
        users::added_bucket_of(&api, users::reauthenticate(&api, &ada, PASSWORD)).await;
    // The proof gave the session new tokens (AUTH-095).
    let ada = SignedIn::from(&first);

    // The re-authentication waits at that bucket while another request ends its session.
    let (status, body) = users::after_its_session_ended(
        &api,
        &ada,
        &checks,
        (
            http::Method::POST,
            "/v1/users/me/sessions/current/reauthenticate",
        ),
        Some(json!({ "password": PASSWORD })),
    )
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    let recorded = db
        .owner_reads(
            "SELECT count(*)::text FROM audit_entries WHERE action = 'session.reauthenticated'",
        )
        .await;
    assert_eq!(recorded, ["1"]);
}

#[tokio::test]
async fn auth_023_a_deletion_needs_the_password_proved_within_5_minutes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 6).await;
    let delete = |access: &str| {
        api.request(http::Method::DELETE, "/v1/users/me")
            .bearer(access)
            .send()
    };

    let refused = delete(&ada.access).await;
    let proved = users::reauthenticate(&api, &ada, PASSWORD).await;
    assert_eq!(proved.status, 200);
    // The proof gave the session new tokens (AUTH-095).
    let deleted = delete(&SignedIn::from(&proved).access).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.reauthentication_required");
    assert_eq!(deleted.status, 204);
    let cleared = deleted.headers("set-cookie");
    for name in ["access_token", "refresh_token"] {
        let cookie =
            format!("__Host-{name}=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict");
        assert!(cleared.contains(&cookie.as_str()), "{cleared:?}");
    }
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM users")
            .await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_023_a_deletion_whose_session_another_request_ended_before_its_check_answers_401() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    // Every bucket of Ada's requests but a deletion's, which a refused deletion then adds.
    api.get("/v1/users/me").bearer(&ada.access).send().await;
    users::authenticated_ago(&api, &ada, 10).await;
    let deletion_bucket = users::added_bucket(&api, users::delete(&api, &ada)).await;
    users::authenticated_ago(&api, &ada, 0).await;

    // The deletion waits at its bucket, once its request is authenticated, while another request
    // ends its session.
    let (status, body) = users::after_its_session_ended(
        &api,
        &ada,
        &deletion_bucket,
        (http::Method::DELETE, "/v1/users/me"),
        None,
    )
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    let users = db
        .owner_reads(&format!(
            "SELECT count(*)::text FROM users WHERE id = '{}'",
            ada.user_id
        ))
        .await;
    assert_eq!(users, ["1"]);
}

#[tokio::test]
async fn auth_023_a_session_that_changed_its_password_within_5_minutes_may_delete_its_account() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 6).await;
    let changed = users::change_password(&api, &ada, PASSWORD, "lantern-orchard-58-brisk").await;
    assert_eq!(changed.status, 200);

    // With the tokens that the change gave the session (AUTH-095).
    let deleted = users::delete(&api, &SignedIn::from(&changed)).await;

    assert_eq!(deleted.status, 204);
}

#[tokio::test]
async fn auth_023_a_session_that_has_just_started_may_delete_its_account() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let deleted = users::delete(&api, &ada).await;

    assert_eq!(deleted.status, 204);
}

#[tokio::test]
async fn auth_024_the_deletions_of_an_email_s_accounts_count_in_one_bucket_of_the_address() {
    // With email off, so the address's six registrations count in no bucket of theirs (AUTH-029),
    // while each deletion still counts in its own.
    let api = Api::without_email().await;
    for _ in 0..5 {
        let ada = users::register(&api, "ada@example.com").await;
        assert_eq!(users::delete(&api, &ada).await.status, 204);
    }
    let ada = users::register(&api, "ada@example.com").await;
    let ada_id = ada.user_id.clone();
    let grace = users::register(&api, "grace@example.com").await;

    let limited = users::delete(&api, &ada).await;
    let other = users::delete(&api, &grace).await;

    assert_eq!(limited.status, 429);
    assert_eq!(limited.json()["code"], "request.rate_limited");
    assert!(
        limited
            .header("ratelimit-policy")
            .starts_with("\"deletion_email\";q=5;w=86400"),
        "{}",
        limited.header("ratelimit-policy")
    );
    assert_eq!(other.status, 204);
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT count(*)::text FROM users WHERE id = '{ada_id}'"
            ))
            .await,
        ["1"]
    );
}

/// What `user`'s deletion answers when another connection holds their row, as a change of their
/// address holds it, and makes their address `grace@example.com`, and commits, while the deletion
/// waits for the row. The deletion's read of the address before its unit of work locks nothing,
/// so it reads the address before the change, and counts in that address's bucket.
async fn deletion_meeting_a_change_of_address(api: &Api, user: &SignedIn) -> Response {
    let db = api.database();
    let mut change = db
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR NO KEY UPDATE",
            user.user_id
        ))
        .await;

    let deletion = tokio::spawn({
        let api = api.clone();
        let access = user.access.clone();
        async move {
            api.request(http::Method::DELETE, "/v1/users/me")
                .bearer(&access)
                .send()
                .await
        }
    });
    // The statement that waits is the deletion's lock of the user's row, in its unit of work.
    query_waits_on_a_lock(db, "FROM users WHERE id = $1 FOR NO KEY UPDATE").await;
    execute_on(
        &mut change,
        &format!(
            "UPDATE users SET email = 'grace@example.com' WHERE id = '{}'",
            user.user_id
        ),
    )
    .await;
    execute_on(&mut change, "COMMIT").await;
    deletion.await.unwrap()
}

#[tokio::test]
async fn auth_024_a_deletion_whose_account_s_address_changed_as_it_waited_answers_429_beyond_the_new_address_s_limit()
 {
    // With email off, so the address's registrations count in no bucket of theirs (AUTH-029),
    // and its accounts delete themselves without proving it (AUTH-025).
    let api = Api::without_email().await;
    for _ in 0..5 {
        let grace = users::register(&api, "grace@example.com").await;
        assert_eq!(users::delete(&api, &grace).await.status, 204);
    }
    let ada = users::register(&api, "ada@example.com").await;

    let limited = deletion_meeting_a_change_of_address(&api, &ada).await;

    assert_eq!(limited.status, 429);
    assert_eq!(limited.json()["code"], "request.rate_limited");
    assert!(
        limited
            .header("ratelimit-policy")
            .starts_with("\"deletion_email\";q=5;w=86400"),
        "{}",
        limited.header("ratelimit-policy")
    );
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT count(*)::text FROM users WHERE id = '{}'",
                ada.user_id
            ))
            .await,
        ["1"]
    );
    let still = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(still.status, 200);
}

#[tokio::test]
async fn auth_024_a_deletion_whose_account_s_address_changed_as_it_waited_counts_in_the_new_address_s_bucket()
 {
    // With email off, as above.
    let api = Api::without_email().await;
    for _ in 0..4 {
        let grace = users::register(&api, "grace@example.com").await;
        assert_eq!(users::delete(&api, &grace).await.status, 204);
    }
    let ada = users::register(&api, "ada@example.com").await;

    let deleted = deletion_meeting_a_change_of_address(&api, &ada).await;
    let grace = users::register(&api, "grace@example.com").await;
    let sixth = users::delete(&api, &grace).await;

    assert_eq!(deleted.status, 204);
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT count(*)::text FROM users WHERE id = '{}'",
                ada.user_id
            ))
            .await,
        ["0"]
    );
    assert_eq!(sixth.status, 429);
    assert!(
        sixth
            .header("ratelimit-policy")
            .starts_with("\"deletion_email\";q=5;w=86400"),
        "{}",
        sixth.header("ratelimit-policy")
    );
}
