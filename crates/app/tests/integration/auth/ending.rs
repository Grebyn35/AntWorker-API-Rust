//! The operator's end of an account's sessions, or of every account's (the auth spec's "A user's
//! sessions", AUTH-094).

use std::process::Output;

use crate::support::{
    Api,
    api::reads,
    audit, execute_on, mailbox,
    operator::{self, SENDS_EMAIL, printed},
    query_waits_on_a_lock,
    second_factors::{self, add_app},
    users::{self, SignedIn},
};

/// Runs the binary's `end-sessions` with `arguments` on `api`'s database.
async fn end(api: &Api, arguments: &[&str]) -> Output {
    let arguments: Vec<&str> = ["end-sessions"]
        .into_iter()
        .chain(arguments.iter().copied())
        .collect();
    api.database().run_api(&arguments, &SENDS_EMAIL).await
}

/// Asserts that `output` exited 1 with the one failure that OPS-03 gives the subcommand, whose
/// message is `message`, and that it never wrote `address`, since the logs hold no personal data
/// (OBS-04).
fn refused(output: &Output, message: &str, address: &str) {
    operator::refused(output, "end-sessions", message, &[address]);
}

/// The log, actor, resource type and resource ID of each `session.ended` entry, the actor `-`
/// when there is none, in order.
async fn ended_entries(api: &Api) -> Vec<String> {
    reads(
        api,
        "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_type \
         || ' ' || coalesce(resource_id::text, '-') FROM audit_entries \
         WHERE action = 'session.ended' ORDER BY 1",
    )
    .await
}

/// What a `session.ended` entry of `user`'s log reads for `session`, as [`ended_entries`] reads
/// one that has no actor.
fn entry(user: &SignedIn, session: &SignedIn) -> String {
    format!("{} - session {}", user.user_id, session.session_id)
}

#[tokio::test]
async fn auth_094_the_subcommand_ends_every_session_of_the_account_that_holds_the_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let second = users::new_session(&api, &address).await;
    let bob = users::register(&api, &mailbox::address("bob")).await;
    add_app(&api, &ada).await;
    // A sign-in of Ada's that waits for its second factor, which the end leaves.
    second_factors::challenge(&api, &address).await;
    let device_tokens = reads(&api, "SELECT id::text FROM device_tokens ORDER BY 1").await;
    let emails = "SELECT id::text FROM jobs WHERE kind = 'platform.send_email' ORDER BY 1";
    let sent = reads(&api, emails).await;
    let mut signals = api.database().signals().await;

    let ended = end(&api, &[&address.to_uppercase()]).await;

    operator::finished(
        &ended,
        "end-sessions",
        "ended every session of the account that holds the address: sessions 2",
        &[&address],
    );
    assert_eq!(users::sessions(&api).await, [bob.session_id.as_str()]);
    let mut expected = vec![entry(&ada, &ada), entry(&ada, &second)];
    expected.sort();
    assert_eq!(ended_entries(&api).await, expected);
    assert_eq!(
        audit::with_a_client(api.database(), "session.ended").await,
        0,
        "an operator's subcommand records no client"
    );
    // Each ended session's event goes to the user's streams (AUTH-046).
    let mut events = vec![
        format!("{} {}", ada.user_id, ada.session_id),
        format!("{} {}", ada.user_id, second.session_id),
    ];
    events.sort();
    assert_eq!(users::session_events(&api, "session.ended").await, events);
    let closed = signals.ended_sessions().await;
    let mut ended_ids = vec![ada.session_id.clone(), second.session_id.clone()];
    ended_ids.sort();
    assert_eq!(closed, ended_ids);
    // It leaves the account's challenge and device tokens, and sends no email.
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM sign_in_challenges").await,
        ["1"]
    );
    assert_eq!(
        reads(&api, "SELECT id::text FROM device_tokens ORDER BY 1").await,
        device_tokens
    );
    assert_eq!(reads(&api, emails).await, sent);
}

#[tokio::test]
async fn auth_094_with_all_the_subcommand_ends_every_session_of_every_account() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let second = users::new_session(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    // Carol holds no session.
    let carol = users::register(&api, "carol@example.com").await;
    let signed_out = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/current")
        .bearer(&carol.access)
        .send()
        .await;
    assert_eq!(signed_out.status, 204);
    let before = ended_entries(&api).await;
    let mut signals = api.database().signals().await;

    let ended = end(&api, &["--all"]).await;

    operator::finished(
        &ended,
        "end-sessions",
        "ended every session of every account: sessions 3, accounts 2",
        &["example.com"],
    );
    assert!(users::sessions(&api).await.is_empty());
    let mut expected = before;
    expected.extend([entry(&ada, &ada), entry(&ada, &second), entry(&bob, &bob)]);
    expected.sort();
    assert_eq!(ended_entries(&api).await, expected);
    assert_eq!(
        reads(
            &api,
            "SELECT count(*)::text FROM audit_entry_clients c JOIN audit_entries e \
             ON e.id = c.audit_entry_id WHERE e.action = 'session.ended' \
             AND e.actor_user_id IS NULL \
             AND (c.client_ip IS NOT NULL OR c.user_agent IS NOT NULL)"
        )
        .await,
        ["0"]
    );
    let closed = signals.ended_sessions().await;
    let mut ended_ids = vec![
        ada.session_id.clone(),
        second.session_id.clone(),
        bob.session_id.clone(),
    ];
    ended_ids.sort();
    assert_eq!(closed, ended_ids);
    // A token of an ended session authenticates nothing.
    let current = api
        .get("/v1/users/me/sessions/current")
        .bearer(&bob.access)
        .send()
        .await;
    assert_eq!(current.status, 401, "{}", current.json());
}

#[tokio::test]
async fn auth_046_the_operator_s_end_publishes_session_ended_for_each_session_it_ends() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let second = users::new_session(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;

    let ended = end(&api, &["--all"]).await;

    assert_eq!(ended.status.code(), Some(0), "{}", printed(&ended));
    let mut events = vec![
        format!("{} {}", ada.user_id, ada.session_id),
        format!("{} {}", ada.user_id, second.session_id),
        format!("{} {}", bob.user_id, bob.session_id),
    ];
    events.sort();
    assert_eq!(users::session_events(&api, "session.ended").await, events);
}

#[tokio::test]
async fn auth_094_with_all_the_subcommand_ends_the_sessions_of_more_users_than_one_read_takes() {
    let api = Api::new().await;
    // 1,001 users with a session each, one more than a read of the users takes.
    users::many_with_a_session(api.database(), 1001).await;

    let ended = end(&api, &["--all"]).await;

    operator::finished(
        &ended,
        "end-sessions",
        "ended every session of every account: sessions 1001, accounts 1001",
        &["example.com"],
    );
    assert!(users::sessions(&api).await.is_empty());
    assert_eq!(
        reads(
            &api,
            "SELECT count(DISTINCT user_id)::text FROM audit_entries \
             WHERE action = 'session.ended'"
        )
        .await,
        ["1001"]
    );
}

#[tokio::test]
async fn auth_094_an_address_with_all_or_neither_changes_nothing_and_exits_2() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let neither = end(&api, &[]).await;
    let both = end(&api, &["ada@example.com", "--all"]).await;

    for output in [&neither, &both] {
        assert_eq!(output.status.code(), Some(2), "{}", printed(output));
    }
    assert_eq!(users::sessions(&api).await, [ada.session_id.as_str()]);
    assert!(ended_entries(&api).await.is_empty());
}

#[tokio::test]
async fn auth_094_an_unknown_address_or_no_session_to_end_changes_nothing_and_exits_1() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let signed_out = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(signed_out.status, 204);
    let nobody = mailbox::address("nobody");
    let before = reads(&api, "SELECT id::text FROM audit_entries ORDER BY id").await;

    let unknown = end(&api, &[&nobody]).await;
    let no_session = end(&api, &[&address]).await;
    let malformed = end(&api, &["not an address"]).await;
    let none_at_all = end(&api, &["--all"]).await;

    refused(
        &unknown,
        "no account holds the address: nothing changed",
        &nobody,
    );
    refused(
        &no_session,
        "the account that holds the address has no session: nothing changed",
        &address,
    );
    refused(
        &malformed,
        "the address given isn't an email address, so no account holds it: nothing changed",
        "not an address",
    );
    refused(
        &none_at_all,
        "no account has a session: nothing changed",
        &address,
    );
    assert_eq!(
        reads(&api, "SELECT id::text FROM audit_entries ORDER BY id").await,
        before
    );
}

#[tokio::test]
async fn auth_094_an_account_that_changes_its_address_while_the_end_waits_keeps_its_sessions() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    // A change of the account's address that hasn't committed holds its row.
    let mut change = api
        .database()
        .holding(&format!(
            "UPDATE users SET email = 'moved-{0}@example.com' WHERE id = '{0}'",
            ada.user_id
        ))
        .await;

    let ending = tokio::spawn({
        let api = api.clone();
        let address = address.clone();
        async move { end(&api, &[&address]).await }
    });
    // The end's read of the account waits for its row.
    query_waits_on_a_lock(api.database(), "FOR NO KEY UPDATE").await;
    execute_on(&mut change, "COMMIT").await;
    let ending = ending.await.unwrap();

    refused(
        &ending,
        "no account holds the address: nothing changed",
        &address,
    );
    assert_eq!(users::sessions(&api).await, [ada.session_id.as_str()]);
}

#[tokio::test]
async fn auth_094_with_all_the_end_of_a_user_s_sessions_waits_for_their_row() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // A request of Ada's that hasn't committed holds her row, as a change of her password does.
    let mut request = api
        .database()
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR NO KEY UPDATE",
            ada.user_id
        ))
        .await;

    let ending = tokio::spawn({
        let api = api.clone();
        async move { end(&api, &["--all"]).await }
    });
    // The end takes the user's turn before it deletes a session of theirs.
    query_waits_on_a_lock(api.database(), "FOR NO KEY UPDATE").await;
    assert_eq!(users::sessions(&api).await, [ada.session_id.as_str()]);
    execute_on(&mut request, "COMMIT").await;
    let ending = ending.await.unwrap();

    operator::finished(
        &ending,
        "end-sessions",
        "ended every session of every account: sessions 1, accounts 1",
        &["example.com"],
    );
    assert!(users::sessions(&api).await.is_empty());
}
