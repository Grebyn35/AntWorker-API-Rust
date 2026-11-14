//! The operator's setup of a workspace, which the binary's subcommand `setup-workspace` runs
//! (the workspaces spec's "The setup subcommand", WORKSPACES-023 to WORKSPACES-028).

use std::process::Output;

use serde_json::json;

use crate::{
    accounts::exports::{built, document},
    support::{
        Api, execute_on, mailbox,
        operator::{EMAIL_OFF, SENDS_EMAIL, finished, refused},
        queries_waiting_on_locks,
        users::{self, PASSWORD},
    },
};

/// The subcommand, which a line of its log names as its area (OPS-03).
const AREA: &str = "setup-workspace";

/// The subject of the email that tells an address the workspace Acme was set up for it.
const TOLD: &str = "The workspace Acme was set up for you on AntWorker";

/// Runs the binary's `setup-workspace` with `arguments` on `api`'s database, in a deployment
/// with the settings `env`.
async fn set_up(api: &Api, env: &[(&str, &str)], arguments: &[&str]) -> Output {
    let arguments: Vec<&str> = [AREA]
        .into_iter()
        .chain(arguments.iter().copied())
        .collect();
    api.database().run_api(&arguments, env).await
}

/// The first column of each row of `sql`, as the `owner` reads it.
async fn reads(api: &Api, sql: &str) -> Vec<String> {
    api.database().owner_reads(sql).await
}

/// Each workspace's ID and name, the oldest first.
async fn workspaces(api: &Api) -> Vec<(String, String)> {
    reads(
        api,
        "SELECT id::text || ' ' || name FROM workspaces ORDER BY id",
    )
    .await
    .iter()
    .map(|row| {
        let (id, name) = row.split_once(' ').unwrap();
        (id.to_owned(), name.to_owned())
    })
    .collect()
}

/// Each membership: its workspace, its user and its role, by workspace.
async fn memberships(api: &Api) -> Vec<String> {
    reads(
        api,
        "SELECT workspace_id || ' ' || user_id || ' ' || role FROM workspace_memberships \
         ORDER BY workspace_id",
    )
    .await
}

/// The log, actor, target, resource and details of each entry of `action`, the oldest first,
/// with `-` for no actor or no target, and whether any of them records a client.
async fn entries(api: &Api, action: &str) -> (Vec<String>, bool) {
    let entries = reads(
        api,
        &format!(
            "SELECT scope || ' ' || coalesce(workspace_id, user_id) || ' ' \
             || coalesce(actor_user_id::text, '-') || ' ' || coalesce(target_user_id::text, '-') \
             || ' ' || resource_type || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = '{action}' ORDER BY id"
        ),
    )
    .await;
    let clients = reads(
        api,
        &format!(
            "SELECT count(*)::text FROM audit_entry_clients c JOIN audit_entries e \
             ON e.id = c.entry_id WHERE e.action = '{action}' \
             AND (c.client_ip IS NOT NULL OR c.user_agent IS NOT NULL)"
        ),
    )
    .await;
    (entries, clients != ["0"])
}

/// The user of each email job that tells an address a workspace was set up, `-` for no one.
async fn email_jobs(api: &Api) -> Vec<String> {
    reads(
        api,
        "SELECT coalesce(user_id::text, '-') FROM jobs WHERE kind = 'platform.send_email' \
         AND payload::text LIKE '%was set up for you%' ORDER BY id",
    )
    .await
}

/// The newest email that tells `address` the workspace Acme was set up for it, once the API's
/// jobs have run.
async fn told(api: &Api, address: &str) -> mailbox::Email {
    api.run_jobs().await;
    mailbox::to(address)
        .await
        .into_iter()
        .find(|email| email.subject == TOLD)
        .expect("the email that tells the address")
}

/// How many rows `table` holds.
async fn count(api: &Api, table: &str) -> String {
    reads(api, &format!("SELECT count(*)::text FROM {table}"))
        .await
        .remove(0)
}

/// Waits until a query that holds `text` waits on a lock: the state a race's outcome depends on,
/// rather than any wait (TEST-06).
async fn waits_on_a_lock(api: &Api, text: &str) {
    while !queries_waiting_on_locks(api.database())
        .await
        .iter()
        .any(|query| query.contains(text))
    {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn workspaces_023_a_run_creates_the_workspace_for_the_account_that_holds_the_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    // The registration's session published its start (AUTH-046).
    api.database().owner_executes("DELETE FROM events").await;
    let mut signals = api.database().signals().await;

    let run = set_up(
        &api,
        &SENDS_EMAIL,
        &["  Acme \t Labs ", &address.to_uppercase()],
    )
    .await;
    let listed = api.get("/v1/workspaces").bearer(&ada.access).send().await;

    let created = workspaces(&api).await;
    assert_eq!(created.len(), 1, "{created:?}");
    let (id, name) = &created[0];
    assert_eq!(name, "Acme Labs");
    finished(
        &run,
        AREA,
        &format!(
            "set up the workspace {id}, whose owner is the account that holds the address, which \
             is told"
        ),
        &["Acme", &address, &address.to_uppercase()],
    );
    let user = &ada.user_id;
    assert_eq!(memberships(&api).await, [format!("{id} {user} owner")]);
    let (recorded, with_client) = entries(&api, "workspace.created").await;
    assert_eq!(
        recorded,
        [format!("workspace {id} - {user} workspace {id} {{}}")]
    );
    assert!(!with_client, "an operator's subcommand records no client");
    let events = reads(
        &api,
        "SELECT name || ' ' || scope || ' ' || user_id || ' ' || (payload->>'workspace_id') \
         || ' ' || coalesce((payload->'session_id')::text, 'absent') FROM events",
    )
    .await;
    assert_eq!(
        events,
        [format!("membership.created user {user} {id} null")]
    );
    let transaction: u64 = reads(&api, "SELECT xact::text FROM events").await[0]
        .parse()
        .unwrap();
    assert_eq!(
        signals.received().await,
        [json!({
            "kind": "membership_changed",
            "user_id": user,
            "workspace_id": id,
            "transaction": transaction
        })]
    );
    // The owner reads the workspace as its owner.
    assert_eq!(listed.json()["items"][0]["id"], id.as_str());
    assert_eq!(listed.json()["items"][0]["role"], "owner");
}

#[tokio::test]
async fn workspaces_024_with_email_on_an_account_that_proved_its_address_becomes_the_owner() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;

    let run = set_up(&api, &SENDS_EMAIL, &["Acme", &address]).await;

    assert_eq!(run.status.code(), Some(0));
    let id = workspaces(&api).await.remove(0).0;
    assert_eq!(
        memberships(&api).await,
        [format!("{id} {} owner", ada.user_id)]
    );
    assert_eq!(count(&api, "users").await, "1");
    assert!(entries(&api, "user.created").await.0.is_empty());
}

#[tokio::test]
async fn workspaces_024_with_email_on_a_run_creates_the_account_when_none_holds_the_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");

    let run = set_up(
        &api,
        &SENDS_EMAIL,
        &["Acme", &address, "  Ada \t Lovelace "],
    )
    .await;

    let id = workspaces(&api).await.remove(0).0;
    finished(
        &run,
        AREA,
        &format!(
            "set up the workspace {id}, and created its owner's account, which has no password; \
             its address is told how to set one"
        ),
        &["Acme", &address, "Lovelace"],
    );
    let accounts = reads(
        &api,
        "SELECT id::text || ' ' || email || ' ' || email_verified || ' ' || name FROM users",
    )
    .await;
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    let user = accounts[0].split(' ').next().unwrap();
    assert_eq!(accounts, [format!("{user} {address} false Ada Lovelace")]);
    assert_eq!(count(&api, "identities").await, "0");
    assert_eq!(count(&api, "provider_identities").await, "0");
    assert_eq!(memberships(&api).await, [format!("{id} {user} owner")]);
    let (recorded, with_client) = entries(&api, "user.created").await;
    assert_eq!(recorded, [format!("user {user} - - user {user} {{}}")]);
    assert!(!with_client, "an operator's subcommand records no client");
}

#[tokio::test]
async fn workspaces_024_the_new_account_s_owner_sets_a_password_through_a_reset_and_signs_in() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let run = set_up(&api, &SENDS_EMAIL, &["Acme", &address, "Ada Lovelace"]).await;
    assert_eq!(run.status.code(), Some(0));
    let id = workspaces(&api).await.remove(0).0;

    let asked = api
        .post("/v1/password-resets", &json!({ "email": address }))
        .send()
        .await;
    api.run_jobs().await;
    let emails = mailbox::to(&address).await;
    let code = emails
        .iter()
        .find_map(|email| mailbox::grouped_code_in(&email.text))
        .expect("a reset code in an email");
    let confirmed = api
        .post(
            "/v1/password-resets/confirm",
            &json!({ "email": address, "code": code, "password": PASSWORD }),
        )
        .send()
        .await;
    let signed_in = users::sign_in(&api, &address, PASSWORD).await;

    assert_eq!(asked.status, 202);
    assert_eq!(confirmed.status, 204, "{}", confirmed.json());
    assert_eq!(signed_in.status, 201, "{}", signed_in.json());
    // The reset proved the address, so the API serves the account.
    assert_eq!(signed_in.json()["email_verification_required"], false);
    let access = signed_in.json()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let read = api
        .get(&format!("/v1/workspaces/{id}"))
        .bearer(&access)
        .send()
        .await;
    assert_eq!(read.status, 200);
    assert_eq!(read.json()["role"], "owner");
}

#[tokio::test]
async fn workspaces_024_an_account_without_an_identity_owns_another_before_its_owner_signs_in() {
    let api = Api::new().await;
    let address = mailbox::address("ada");

    let first = set_up(&api, &SENDS_EMAIL, &["Acme", &address, "Ada Lovelace"]).await;
    // The account exists, so the second run needs no name for it.
    let second = set_up(&api, &SENDS_EMAIL, &["Globex", &address]).await;

    assert_eq!(first.status.code(), Some(0));
    let created = workspaces(&api).await;
    assert_eq!(created.len(), 2, "{created:?}");
    finished(
        &second,
        AREA,
        &format!(
            "set up the workspace {}, whose owner is the account that holds the address, which \
             has no password yet; its address is told how to set one",
            created[1].0
        ),
        &["Globex", &address],
    );
    let user = reads(&api, "SELECT id::text FROM users").await;
    assert_eq!(user.len(), 1);
    assert_eq!(
        memberships(&api).await,
        [
            format!("{} {} owner", created[0].0, user[0]),
            format!("{} {} owner", created[1].0, user[0])
        ]
    );
    assert_eq!(entries(&api, "user.created").await.0.len(), 1);
}

#[tokio::test]
async fn workspaces_024_an_account_that_a_deletion_takes_while_the_run_waits_leaves_its_address() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let gone = users::without_password(api.database(), &address).await;
    // A deletion of the account that hasn't committed holds its row.
    let mut deletion = api
        .database()
        .holding(&format!("DELETE FROM users WHERE id = '{gone}'"))
        .await;

    let run = tokio::spawn({
        let api = api.clone();
        let address = address.clone();
        async move { set_up(&api, &SENDS_EMAIL, &["Acme", &address, "Ada Lovelace"]).await }
    });
    waits_on_a_lock(&api, "FOR NO KEY UPDATE").await;
    execute_on(&mut deletion, "COMMIT").await;
    let run = run.await.unwrap();

    let id = workspaces(&api).await.remove(0).0;
    finished(
        &run,
        AREA,
        &format!("set up the workspace {id}, and created its owner's account"),
        &[&address],
    );
    let user = reads(&api, "SELECT id::text FROM users").await;
    assert_eq!(user.len(), 1);
    assert_ne!(user[0], gone);
    assert_eq!(memberships(&api).await, [format!("{id} {} owner", user[0])]);
}

#[tokio::test]
async fn workspaces_025_with_email_on_a_run_tells_the_address_which_workspace_and_how_to_sign_in() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;

    let run = set_up(&api, &SENDS_EMAIL, &["Acme", &address]).await;

    assert_eq!(run.status.code(), Some(0));
    // The job runs for the account that owns the workspace (PLATFORM-013).
    assert_eq!(email_jobs(&api).await, [ada.user_id]);
    let email = told(&api, &address).await;
    for part in [&email.text, &email.html] {
        assert!(part.contains("the workspace Acme for you"), "{part}");
        assert!(part.contains("you are its owner"), "{part}");
        assert!(part.contains("Sign in to AntWorker with"), "{part}");
        assert!(part.contains(&address), "{part}");
        assert!(!part.contains("http"), "{part}");
    }
}

#[tokio::test]
async fn workspaces_025_the_email_tells_an_account_without_an_identity_how_to_set_a_password() {
    let api = Api::new().await;
    let address = mailbox::address("ada");

    let run = set_up(&api, &SENDS_EMAIL, &["Acme", &address, "Ada Lovelace"]).await;

    assert_eq!(run.status.code(), Some(0));
    let user = reads(&api, "SELECT id::text FROM users").await;
    assert_eq!(email_jobs(&api).await, user);
    let email = told(&api, &address).await;
    for part in [&email.text, &email.html] {
        assert!(part.contains("the workspace Acme for you"), "{part}");
        assert!(part.contains("it has no password yet"), "{part}");
        assert!(part.contains("ask for a password reset"), "{part}");
        assert!(part.contains(&address), "{part}");
        assert!(!part.contains("http"), "{part}");
        assert!(mailbox::grouped_code_in(part).is_none(), "{part}");
    }
}

#[tokio::test]
async fn workspaces_025_with_email_off_a_run_enqueues_no_email() {
    let api = Api::without_email().await;
    users::register_unverified(&api, "ada@example.com").await;

    let run = set_up(&api, &EMAIL_OFF, &["Acme", "ada@example.com"]).await;

    assert_eq!(run.status.code(), Some(0));
    assert_eq!(
        reads(
            &api,
            "SELECT count(*)::text FROM jobs WHERE kind = 'platform.send_email'"
        )
        .await,
        ["0"]
    );
}

#[tokio::test]
async fn workspaces_026_with_email_off_the_account_that_holds_the_address_owns_the_workspace() {
    let api = Api::without_email().await;
    // With email off, nothing proves an address.
    let ada = users::register_unverified(&api, "ada@example.com").await;

    let run = set_up(&api, &EMAIL_OFF, &["Acme", "ada@example.com"]).await;

    let id = workspaces(&api).await.remove(0).0;
    finished(
        &run,
        AREA,
        &format!(
            "set up the workspace {id}, whose owner is the account that holds the address; email \
             is off, so its address isn't told"
        ),
        &["Acme", "ada@example.com"],
    );
    assert_eq!(
        memberships(&api).await,
        [format!("{id} {} owner", ada.user_id)]
    );
    assert!(email_jobs(&api).await.is_empty());
    assert_eq!(count(&api, "users").await, "1");
}

#[tokio::test]
async fn workspaces_026_with_email_off_a_run_creates_no_account_and_takes_none_without_identity() {
    let api = Api::without_email().await;
    users::without_password(api.database(), "grace@example.com").await;

    let nobody = set_up(
        &api,
        &EMAIL_OFF,
        &["Acme", "ada@example.com", "Ada Lovelace"],
    )
    .await;
    let unsigned = set_up(&api, &EMAIL_OFF, &["Globex", "grace@example.com"]).await;

    refused(
        &nobody,
        AREA,
        "no account holds the address, and with email off the subcommand creates none, since its \
         owner could set no password: nothing changed; run it again once the owner has registered",
        &["Acme", "ada@example.com", "Lovelace"],
    );
    refused(
        &unsigned,
        AREA,
        "the account that holds the address has no password and no sign-in provider, and with \
         email off its owner can set no password: nothing changed",
        &["Globex", "grace@example.com"],
    );
    assert_eq!(count(&api, "users").await, "1");
    assert_eq!(count(&api, "workspaces").await, "0");
    assert_eq!(count(&api, "workspace_setups").await, "0");
}

#[tokio::test]
async fn workspaces_027_a_name_that_an_earlier_run_was_given_sets_nothing_up() {
    let api = Api::new().await;
    let ada = mailbox::address("ada");
    let grace = mailbox::address("grace");
    let first = set_up(&api, &SENDS_EMAIL, &["Acme  Labs", &ada, "Ada Lovelace"]).await;
    assert_eq!(first.status.code(), Some(0));
    let audited = count(&api, "audit_entries").await;

    // The same name once it is normalized, for another owner, whose account it would create.
    let again = set_up(&api, &SENDS_EMAIL, &[" Acme Labs ", &grace, "Grace Hopper"]).await;

    refused(
        &again,
        AREA,
        "an earlier run was given the name, and set its workspace up: nothing changed",
        &["Acme", &grace, "Hopper"],
    );
    assert_eq!(count(&api, "workspaces").await, "1");
    assert_eq!(count(&api, "users").await, "1");
    assert_eq!(count(&api, "workspace_memberships").await, "1");
    assert_eq!(count(&api, "audit_entries").await, audited);
    assert_eq!(email_jobs(&api).await.len(), 1);
    assert_eq!(
        reads(&api, "SELECT name FROM workspace_setups").await,
        ["Acme Labs"]
    );
}

#[tokio::test]
async fn workspaces_027_of_two_runs_at_once_with_one_name_one_sets_its_workspace_up() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    // A run that hasn't committed holds the name.
    let mut other = api
        .database()
        .holding(
            "WITH created AS (INSERT INTO workspaces (name) VALUES ('Acme') RETURNING id) \
             INSERT INTO workspace_setups (workspace_id, name) SELECT id, 'Acme' FROM created",
        )
        .await;

    let run = tokio::spawn({
        let api = api.clone();
        let address = address.clone();
        async move { set_up(&api, &SENDS_EMAIL, &["Acme", &address, "Ada Lovelace"]).await }
    });
    waits_on_a_lock(&api, "INSERT INTO workspace_setups").await;
    execute_on(&mut other, "COMMIT").await;
    let run = run.await.unwrap();

    refused(
        &run,
        AREA,
        "an earlier run was given the name, and set its workspace up: nothing changed",
        &["Acme", &address],
    );
    assert_eq!(count(&api, "workspaces").await, "1");
    assert_eq!(count(&api, "users").await, "0");
    assert!(email_jobs(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_027_a_rename_keeps_the_name_used_and_a_deletion_frees_it() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let first = set_up(&api, &SENDS_EMAIL, &["Acme", &address]).await;
    assert_eq!(first.status.code(), Some(0));
    let id = workspaces(&api).await.remove(0).0;

    let renamed = api
        .patch(
            &format!("/v1/workspaces/{id}"),
            &json!({ "name": "Globex" }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    let while_renamed = set_up(&api, &SENDS_EMAIL, &["Acme", &address]).await;
    let deleted = api
        .request(http::Method::DELETE, &format!("/v1/workspaces/{id}"))
        .bearer(&ada.access)
        .send()
        .await;
    let once_deleted = set_up(&api, &SENDS_EMAIL, &["Acme", &address]).await;

    assert_eq!(renamed.status, 200, "{}", renamed.json());
    refused(
        &while_renamed,
        AREA,
        "an earlier run was given the name",
        &["Acme"],
    );
    assert_eq!(deleted.status, 204);
    assert_eq!(once_deleted.status.code(), Some(0));
    let created = workspaces(&api).await;
    assert_eq!(created.len(), 1, "{created:?}");
    assert_ne!(created[0].0, id);
    assert_eq!(created[0].1, "Acme");
}

#[tokio::test]
async fn workspaces_027_an_export_of_its_only_member_holds_the_name_the_run_was_given() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let grace = users::register(&api, &mailbox::address("grace")).await;
    let run = set_up(&api, &SENDS_EMAIL, &["Acme", &address]).await;
    assert_eq!(run.status.code(), Some(0));
    let id = workspaces(&api).await.remove(0).0;
    // A workspace that Ada shares, whose setup her export leaves out.
    let shared = set_up(&api, &SENDS_EMAIL, &["Globex", &address]).await;
    assert_eq!(shared.status.code(), Some(0));
    let globex = workspaces(&api).await.remove(1).0;
    crate::support::workspaces::join(api.database(), &globex, &grace.user_id, "member").await;

    let export = built(&api, &ada).await;
    let archive = api
        .get(&format!("/v1/users/me/exports/{export}/content"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(archive.status, 200);
    let setups = document(&archive.body, "workspaces/workspace_setups.json");
    assert_eq!(
        setups,
        [json!({
            "workspace_id": id,
            "name": "Acme",
            "created_at": setups[0]["created_at"],
        })]
    );
}

#[tokio::test]
async fn workspaces_028_what_the_operator_gives_must_name_a_workspace_and_its_owner() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let long = "a".repeat(256);

    let no_name = set_up(&api, &SENDS_EMAIL, &[" \t ", &address, "Ada Lovelace"]).await;
    let long_name = set_up(&api, &SENDS_EMAIL, &[&long, &address, "Ada Lovelace"]).await;
    let no_address = set_up(&api, &SENDS_EMAIL, &["Acme", "not an address", "Ada"]).await;
    let no_owner_name = set_up(&api, &SENDS_EMAIL, &["Acme", &address, "  "]).await;
    let missing = set_up(&api, &SENDS_EMAIL, &["Acme", &address]).await;

    for run in [&no_name, &long_name] {
        refused(
            run,
            AREA,
            "the name given isn't a workspace's name, of 1 to 255 characters: nothing changed",
            &[&address, &long, "Lovelace"],
        );
    }
    refused(
        &no_address,
        AREA,
        "the address given isn't an email address: nothing changed",
        &["Acme", "not an address"],
    );
    refused(
        &no_owner_name,
        AREA,
        "the owner's name given isn't a name, of 1 to 100 characters: nothing changed",
        &["Acme", &address],
    );
    refused(
        &missing,
        AREA,
        "no account holds the address, and the account to create needs its owner's name, which \
         wasn't given: nothing changed",
        &["Acme", &address],
    );
    for table in ["users", "workspaces", "workspace_setups", "audit_entries"] {
        assert_eq!(count(&api, table).await, "0", "{table}");
    }
}

#[tokio::test]
async fn workspaces_028_with_email_on_an_account_that_has_not_proved_its_address_owns_nothing() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register_unverified(&api, &address).await;
    let audited = count(&api, "audit_entries").await;

    let run = set_up(&api, &SENDS_EMAIL, &["Acme", &address, "Ada Lovelace"]).await;

    refused(
        &run,
        AREA,
        "the account that holds the address hasn't proved it, so it may not be its owner's: \
         nothing changed; run it again once the account has proved the address",
        &["Acme", &address],
    );
    assert_eq!(count(&api, "workspaces").await, "0");
    assert_eq!(count(&api, "workspace_setups").await, "0");
    assert_eq!(count(&api, "audit_entries").await, audited);
    assert!(email_jobs(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_028_an_account_that_takes_the_address_while_the_run_creates_one_stops_it() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    // A registration that hasn't committed holds the address.
    let mut registration = api
        .database()
        .holding(&format!(
            "INSERT INTO users (email, name) VALUES ('{address}', 'Ada Lovelace')"
        ))
        .await;

    let run = tokio::spawn({
        let api = api.clone();
        let address = address.clone();
        async move { set_up(&api, &SENDS_EMAIL, &["Acme", &address, "Ada Lovelace"]).await }
    });
    waits_on_a_lock(&api, "INSERT INTO users").await;
    execute_on(&mut registration, "COMMIT").await;
    let run = run.await.unwrap();

    refused(
        &run,
        AREA,
        "an account took the address while the subcommand ran: nothing changed; run it again",
        &["Acme", &address],
    );
    assert_eq!(count(&api, "users").await, "1");
    assert_eq!(count(&api, "workspaces").await, "0");
    assert_eq!(count(&api, "audit_entries").await, "0");
}
