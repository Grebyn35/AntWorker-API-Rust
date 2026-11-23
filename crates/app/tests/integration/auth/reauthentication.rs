//! Re-authenticating, and the deletion of an account that it confirms (the auth spec's
//! "Re-authentication" and "Deleting an account").

use serde_json::json;

use crate::support::{
    Api, execute_on, queries_wait_on_locks,
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
    api.database()
        .owner_executes(&format!(
            "DELETE FROM identities WHERE user_id = '{}'",
            ada.user_id
        ))
        .await;

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
    let mut ender = db
        .holding(&format!(
            "SELECT 1 FROM rate_limit_buckets WHERE id = '{checks}' FOR UPDATE"
        ))
        .await;

    let again = tokio::spawn({
        let api = api.clone();
        let access = ada.access.clone();
        async move {
            let response = api
                .post(
                    "/v1/users/me/sessions/current/reauthenticate",
                    &json!({ "password": PASSWORD }),
                )
                .bearer(&access)
                .send()
                .await;
            (response.status, response.json())
        }
    });
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut ender,
        &format!("DELETE FROM sessions WHERE id = '{}'", ada.session_id),
    )
    .await;
    execute_on(&mut ender, "COMMIT").await;
    let (status, body) = again.await.unwrap();

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
    let mut ender = db
        .holding(&format!(
            "SELECT 1 FROM rate_limit_buckets WHERE id = '{deletion_bucket}' FOR UPDATE"
        ))
        .await;

    let deletion = tokio::spawn({
        let api = api.clone();
        let access = ada.access.clone();
        async move {
            let response = api
                .request(http::Method::DELETE, "/v1/users/me")
                .bearer(&access)
                .send()
                .await;
            (response.status, response.json())
        }
    });
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut ender,
        &format!("DELETE FROM sessions WHERE id = '{}'", ada.session_id),
    )
    .await;
    execute_on(&mut ender, "COMMIT").await;
    let (status, body) = deletion.await.unwrap();

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
    let changed = api
        .post(
            "/v1/users/me/password/change",
            &json!({ "current_password": PASSWORD, "password": "lantern-orchard-58-brisk" }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(changed.status, 200);

    // With the tokens that the change gave the session (AUTH-095).
    let deleted = api
        .request(http::Method::DELETE, "/v1/users/me")
        .bearer(&SignedIn::from(&changed).access)
        .send()
        .await;

    assert_eq!(deleted.status, 204);
}

#[tokio::test]
async fn auth_023_a_session_that_has_just_started_may_delete_its_account() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let deleted = api
        .request(http::Method::DELETE, "/v1/users/me")
        .bearer(&ada.access)
        .send()
        .await;

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
