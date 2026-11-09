//! Deleting an account (the accounts spec's "Deleting an account").

use serde_json::json;

use std::{collections::BTreeSet, sync::Arc};

use platform::{
    clock::TestClock,
    mail::{Mail, MailConfig},
};

use crate::support::{
    Api, Options, TestDatabase,
    api::Response,
    events::{self, Event},
    execute_on, mailbox, queries_wait_on_locks, streams,
    users::{self, SignedIn},
    workspaces,
};
// part: files
use crate::support::files;
// end part: files
// part: invitations
use crate::support::invitations;
// end part: invitations

/// Deletes `user`'s account, through a session that has just proved their password.
async fn delete(api: &Api, user: &SignedIn) -> Response {
    api.request(http::Method::DELETE, "/v1/users/me")
        .bearer(&user.access)
        .send()
        .await
}

/// Each column that references `users`, as `table.column`, with how many of its rows name `user`,
/// for each that any does (DATA-23).
async fn naming(api: &Api, user: &str) -> Vec<String> {
    let db = api.database();
    let columns = db
        .owner_reads(
            "SELECT constraints.conrelid::regclass::text || '.' || columns.attname \
             FROM pg_constraint constraints \
             JOIN pg_attribute columns ON columns.attrelid = constraints.conrelid \
                 AND columns.attnum = constraints.conkey[1] \
             WHERE constraints.contype = 'f' AND constraints.confrelid = 'users'::regclass \
             ORDER BY 1",
        )
        .await;
    assert!(columns.len() >= 10, "{columns:?}");
    let mut naming = Vec::new();
    for column in columns {
        let (table, name) = column.split_once('.').unwrap();
        let count = db
            .owner_reads(&format!(
                "SELECT count(*)::text FROM {table} WHERE {name} = '{user}'"
            ))
            .await
            .remove(0);
        if count != "0" {
            naming.push(format!("{column} {count}"));
        }
    }
    naming
}

/// The one value that `sql` reads.
async fn one(api: &Api, sql: &str) -> String {
    api.database().owner_reads(sql).await.remove(0)
}

#[tokio::test]
async fn accounts_012_a_deletion_is_refused_while_the_user_owns_a_workspace_with_another_member() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let shared = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&bob.user_id, "member")],
    )
    .await;
    workspaces::arrange(db, "Mine", &[(&ada.user_id, "owner")]).await;

    let response = delete(&api, &ada).await;

    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "auth.owns_shared_workspace");
    let ada_id = &ada.user_id;
    assert_eq!(
        one(
            &api,
            &format!(
                "SELECT (SELECT count(*) FROM users WHERE id = '{ada_id}') || ' ' \
                 || (SELECT count(*) FROM workspace_memberships WHERE user_id = '{ada_id}') || ' ' \
                 || (SELECT count(*) FROM workspace_memberships WHERE workspace_id = '{shared}')"
            )
        )
        .await,
        "1 2 2"
    );
    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(me.status, 200);
}

#[tokio::test]
async fn accounts_012_a_workspace_whose_only_member_is_the_user_refuses_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mine = workspaces::arrange(api.database(), "Mine", &[(&ada.user_id, "owner")]).await;

    let response = delete(&api, &ada).await;

    assert_eq!(response.status, 204);
    assert_eq!(
        one(
            &api,
            &format!("SELECT count(*)::text FROM workspaces WHERE id = '{mine}'")
        )
        .await,
        "0"
    );
}

#[tokio::test]
async fn accounts_013_a_deletion_erases_the_account_and_the_workspaces_only_it_belongs_to() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let other_device = users::new_session(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    // A workspace Ada created herself, with its idempotency record, its event and its log.
    let created = api
        .post("/v1/workspaces", &json!({ "name": "Mine" }))
        .bearer(&ada.access)
        .header("idempotency-key", "\"k-1\"")
        .send()
        .await;
    assert_eq!(created.status, 201);
    let mine = created.json()["id"].as_str().unwrap().to_owned();
    // A job of that workspace from a member who has since left, and one enqueued for Ada.
    db.owner_executes(&format!(
        "INSERT INTO jobs (kind, payload, user_id, workspace_id) VALUES \
         ('tests.kept', '{{}}', '{}', '{mine}'), ('tests.kept', '{{}}', '{}', NULL)",
        bob.user_id, ada.user_id
    ))
    .await;
    let shared = workspaces::arrange(
        db,
        "Acme",
        &[(&bob.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    // part: files
    files::arrange(db, &mine, "plan.txt").await;
    // end part: files
    let mut stream = streams::opened(&api, &other_device).await;

    let response = delete(&api, &ada).await;
    stream.closed().await;
    api.run_jobs().await;

    assert_eq!(response.status, 204);
    let reconnect = streams::open_as(&api, &other_device.access).await;
    assert_eq!(
        reconnect.refused().map(|refused| refused.status.as_u16()),
        Some(401)
    );
    let ada_id = &ada.user_id;
    assert!(naming(&api, ada_id).await.is_empty());
    assert_eq!(
        one(
            &api,
            &format!(
                "SELECT (SELECT count(*) FROM users) || ' ' \
                 || (SELECT count(*) FROM workspaces WHERE id = '{mine}') || ' ' \
                 || (SELECT count(*) FROM jobs WHERE workspace_id = '{mine}' OR user_id = '{ada_id}') \
                 || ' ' || (SELECT count(*) FROM jobs WHERE payload::text LIKE '%ada@example.com%') \
                 || ' ' || (SELECT count(*) FROM workspace_memberships WHERE workspace_id = '{shared}')"
            )
        )
        .await,
        "1 0 0 0 1"
    );
    // part: files
    assert_eq!(
        one(&api, "SELECT count(*)::text FROM workspace_files").await,
        "0"
    );
    // end part: files
    for user in [&ada, &other_device] {
        let refused = api.get("/v1/users/me").bearer(&user.access).send().await;
        assert_eq!(refused.status, 401);
    }
    let again = users::sign_in(&api, "ada@example.com", users::PASSWORD).await;
    assert_eq!(again.status, 401);
}

/// Ada, an admin of Bob's workspace, who renamed it and removed Carol from it.
struct Shared {
    api: Api,
    ada: SignedIn,
    bob: SignedIn,
    carol: SignedIn,
    workspace: String,
}

/// Bob's workspace, of which Ada is an admin, and in which she acted.
async fn shared() -> Shared {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[
            (&bob.user_id, "owner"),
            (&ada.user_id, "admin"),
            (&carol.user_id, "member"),
        ],
    )
    .await;
    let renamed = api
        .patch(
            &format!("/v1/workspaces/{workspace}"),
            &json!({ "name": "Acme Inc" }),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(renamed.status, 200);
    let removed = api
        .request(
            http::Method::DELETE,
            &format!("/v1/workspaces/{workspace}/members/{}", carol.user_id),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(removed.status, 204);
    Shared {
        api,
        ada,
        bob,
        carol,
        workspace,
    }
}

#[tokio::test]
async fn accounts_014_leaving_a_shared_workspace_records_and_publishes_member_left() {
    let Shared {
        api,
        ada,
        bob,
        workspace,
        ..
    } = shared().await;
    let mut stream = streams::opened(&api, &bob).await;

    let response = delete(&api, &ada).await;
    let left = stream.event().await;

    assert_eq!(response.status, 204);
    assert_eq!(left.event.as_deref(), Some("member.left"));
    assert_eq!(
        left.payload(),
        &json!({ "workspace_id": workspace, "user_id": ada.user_id })
    );
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT user_id::text FROM workspace_memberships WHERE workspace_id = '{workspace}'"
            ))
            .await,
        std::slice::from_ref(&bob.user_id)
    );
}

#[tokio::test]
async fn accounts_014_the_workspace_s_log_keeps_each_entry_without_the_user_s_id() {
    let Shared {
        api,
        ada,
        carol,
        workspace,
        ..
    } = shared().await;

    let response = delete(&api, &ada).await;

    assert_eq!(response.status, 204);
    let entries = api
        .database()
        .owner_reads(&format!(
            "SELECT entry.action || ' ' || coalesce(entry.actor_user_id::text, '-') || ' ' \
             || coalesce(entry.target_user_id::text, '-') || ' ' || entry.resource_type || ' ' \
             || entry.resource_id || ' ' || (client.id IS NOT NULL)::text \
             FROM audit_entries entry \
             LEFT JOIN audit_entry_clients client ON client.entry_id = entry.id \
             WHERE entry.workspace_id = '{workspace}' ORDER BY entry.created_at, entry.id"
        ))
        .await;
    let carol_id = &carol.user_id;
    assert_eq!(
        entries,
        [
            format!("workspace.renamed - - workspace {workspace} false"),
            format!("member.removed - {carol_id} workspace {workspace} false"),
            format!("member.left - - workspace {workspace} false"),
        ]
    );
    let ada_id = &ada.user_id;
    assert_eq!(
        one(
            &api,
            &format!(
                "SELECT count(*)::text FROM audit_entries entry WHERE entry::text LIKE '%{ada_id}%'"
            )
        )
        .await,
        "0"
    );
    assert!(naming(&api, ada_id).await.is_empty());
}

#[tokio::test]
async fn sec_19_member_left_reaches_the_members_and_no_replay_from_before_joining() {
    let api = Api::new().await;
    let grace = users::register(&api, "grace@example.com").await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(
        db,
        "Acme",
        &[
            (&grace.user_id, "owner"),
            (&ada.user_id, "member"),
            (&bob.user_id, "member"),
        ],
    )
    .await;
    // An event to Alan alone gives his stream one to resume after.
    db.owner_executes(&events::statement(
        &[Event::to_user(&alan.user_id, "note.made", "{}")],
        true,
    ))
    .await;
    let before = streams::opened(&api, &alan).await.last_event_id;
    // Every role holds members.read, which member.left requires: no member lacks it.
    let mut member = streams::opened(&api, &grace).await;

    let first = delete(&api, &ada).await;
    workspaces::join(db, &acme, &alan.user_id, "member").await;
    let mut joined = streams::connect(&api, &alan.access, before.as_deref()).await;
    joined.opened().await;
    let second = delete(&api, &bob).await;
    let after_joining = joined.event().await;

    assert_eq!(first.status, 204);
    assert_eq!(second.status, 204);
    for user in [&ada, &bob] {
        let event = member.event().await;
        assert_eq!(event.event.as_deref(), Some("member.left"));
        assert_eq!(
            event.payload(),
            &json!({ "workspace_id": acme, "user_id": user.user_id })
        );
    }
    // Ada's leaving, from before Alan joined, isn't replayed to him: the first he receives is
    // Bob's, from after.
    assert_eq!(after_joining.event.as_deref(), Some("member.left"));
    assert_eq!(
        after_joining.payload(),
        &json!({ "workspace_id": acme, "user_id": bob.user_id })
    );
}

#[tokio::test]
async fn accounts_015_with_email_on_the_address_hears_that_the_account_is_deleted() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;

    let response = delete(&api, &ada).await;
    let job = one(
        &api,
        "SELECT coalesce(user_id::text, '-') || ' ' || coalesce(workspace_id::text, '-') \
         FROM jobs WHERE kind = 'platform.send_email'",
    )
    .await;
    api.run_jobs().await;

    assert_eq!(response.status, 204);
    // A job for no one: the user it would name is gone.
    assert_eq!(job, "- -");
    let emails = mailbox::to(&address).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject, "Your AntWorker account is deleted");
    assert!(emails[0].text.contains(&address), "{}", emails[0].text);
}

#[tokio::test]
async fn accounts_015_with_email_off_nothing_is_sent() {
    let api = Api::with(Options::default().set("EMAIL", "off")).await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = delete(&api, &ada).await;

    assert_eq!(response.status, 204);
    assert_eq!(one(&api, "SELECT count(*)::text FROM jobs").await, "0");
}

/// The user's row, sessions and memberships, as `count|count|count`, which a deletion that
/// changed nothing leaves.
async fn left_of(api: &Api, user: &SignedIn) -> String {
    let id = &user.user_id;
    one(
        api,
        &format!(
            "SELECT (SELECT count(*) FROM users WHERE id = '{id}') || '|' \
             || (SELECT count(*) FROM sessions WHERE user_id = '{id}') || '|' \
             || (SELECT count(*) FROM workspace_memberships WHERE user_id = '{id}')"
        ),
    )
    .await
}

#[tokio::test]
async fn accounts_016_a_membership_that_a_request_added_while_the_deletion_ran_refuses_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let joined = workspaces::arrange(db, "Acme", &[(&bob.user_id, "owner")]).await;
    // A request of Ada's that adds her to a workspace while the deletion runs: its membership's
    // foreign key holds her row `FOR KEY SHARE`, which the deletion's last step waits for.
    let mut held = db
        .holding(&format!(
            "INSERT INTO workspace_memberships (workspace_id, user_id, role) \
             VALUES ('{joined}', '{}', 'member')",
            ada.user_id
        ))
        .await;

    let (response, ()) = tokio::join!(delete(&api, &ada), async {
        queries_wait_on_locks(db, 1).await;
        execute_on(&mut held, "COMMIT").await;
    });

    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "auth.account_changed");
    assert_eq!(left_of(&api, &ada).await, "1|1|1");
    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(me.status, 200);
}

#[tokio::test]
async fn accounts_016_a_row_that_another_request_holds_locked_refuses_the_deletion_at_once() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    // Another request holds an entry of Ada's own log, which deleting her row deletes.
    let mut held = db
        .holding(&format!(
            "SELECT 1 FROM audit_entries WHERE user_id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    // The lock is held until the deletion has answered, so a deletion that waited for it would
    // never answer.
    let response = delete(&api, &ada).await;
    execute_on(&mut held, "ROLLBACK").await;

    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "auth.account_changed");
    assert_eq!(left_of(&api, &ada).await, "1|1|0");
}

#[tokio::test]
async fn accounts_019_a_deletion_that_waited_for_another_of_the_account_answers_204() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let other_device = users::new_session(&api, "ada@example.com").await;
    let db = api.database();
    // The first deletion holds Ada's row, and waits at her identity, which a sign-in holds.
    let mut held = db
        .holding(&format!(
            "SELECT 1 FROM identities WHERE user_id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    let first = delete(&api, &ada);
    let second = async {
        queries_wait_on_locks(db, 1).await;
        let second = delete(&api, &other_device);
        let release = async {
            queries_wait_on_locks(db, 2).await;
            execute_on(&mut held, "ROLLBACK").await;
        };
        tokio::join!(second, release).0
    };
    let (first, second) = tokio::join!(first, second);

    assert_eq!(first.status, 204);
    assert_eq!(second.status, 204);
    assert_eq!(one(&api, "SELECT count(*)::text FROM users").await, "0");
}

#[tokio::test]
async fn data_23_every_column_that_names_a_user_or_holds_personal_data_has_a_contract() {
    let db = TestDatabase::migrated().await;
    let schema: BTreeSet<String> = db
        .owner_reads(
            "SELECT constraints.conrelid::regclass::text || '.' || columns.attname \
             FROM pg_constraint constraints \
             JOIN pg_attribute columns ON columns.attrelid = constraints.conrelid \
                 AND columns.attnum = ANY(constraints.conkey) \
             WHERE constraints.contype = 'f' AND constraints.confrelid = 'users'::regclass \
             UNION \
             SELECT tables.relname || '.' || columns.attname \
             FROM pg_description comments \
             JOIN pg_class tables ON tables.oid = comments.objoid \
             JOIN pg_attribute columns ON columns.attrelid = tables.oid \
                 AND columns.attnum = comments.objsubid \
             WHERE comments.description = 'personal_data' \
                 AND tables.relnamespace = 'public'::regnamespace",
        )
        .await
        .into_iter()
        .collect();
    let mail = Mail::new(MailConfig::Off, Arc::new(TestClock::default())).unwrap();
    let registered: Vec<String> = app::personal_data_columns(&mail)
        .into_iter()
        .map(|(table, column)| format!("{table}.{column}"))
        .collect();
    let covered: BTreeSet<String> = registered.iter().cloned().collect();

    assert_eq!(
        covered.len(),
        registered.len(),
        "a column covered twice: {registered:?}"
    );
    assert_eq!(
        schema.difference(&covered).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "columns that no contract covers"
    );
    assert_eq!(
        covered.difference(&schema).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "columns that contracts name and the schema lacks"
    );
}

// part: files
#[tokio::test]
async fn accounts_017_the_user_s_own_files_go_and_their_uploads_to_a_shared_workspace_stay() {
    let Shared {
        api,
        ada,
        workspace,
        ..
    } = shared().await;
    let db = api.database();
    files::arrange_own(db, &ada.user_id, "notes.txt").await;
    files::arrange_avatar(db, &ada.user_id).await;
    let uploaded = files::arrange_uploaded(db, &workspace, "plan.txt", &ada.user_id).await;

    let response = delete(&api, &ada).await;

    assert_eq!(response.status, 204);
    assert_eq!(
        one(
            &api,
            &format!(
                "SELECT (SELECT count(*) FROM user_files) || ' ' || (SELECT count(*) FROM avatars) \
                 || ' ' || (SELECT coalesce(uploader_id::text, '-') FROM workspace_files \
                     WHERE id = '{uploaded}')"
            )
        )
        .await,
        "0 0 -"
    );
}

#[tokio::test]
async fn files_001_a_file_whose_uploader_s_account_is_deleted_has_no_uploader() {
    let Shared {
        api,
        ada,
        bob,
        workspace,
        ..
    } = shared().await;
    let file = files::arrange_uploaded(api.database(), &workspace, "plan.txt", &ada.user_id).await;

    let deleted = delete(&api, &ada).await;
    let read = api
        .get(&format!("/v1/workspaces/{workspace}/files/{file}"))
        .bearer(&bob.access)
        .send()
        .await;

    assert_eq!(deleted.status, 204);
    assert_eq!(read.status, 200);
    assert_eq!(read.json()["uploader_id"], json!(null));
}
// end part: files

// part: invitations
#[tokio::test]
async fn accounts_018_the_invitations_the_user_sent_and_received_stay() {
    let Shared {
        api,
        ada,
        carol,
        workspace,
        ..
    } = shared().await;
    let sent = invitations::invite(&api, &ada, &workspace, "dan@example.com", "member").await;
    assert_eq!(sent.status, 201);
    let labs = workspaces::arrange(api.database(), "Labs", &[(&carol.user_id, "owner")]).await;
    let received = invitations::invite(&api, &carol, &labs, "ada@example.com", "member").await;
    assert_eq!(received.status, 201);

    let response = delete(&api, &ada).await;

    assert_eq!(response.status, 204);
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT email || ' ' || coalesce(inviter_id::text, '-') FROM invitations \
                 ORDER BY email"
            )
            .await,
        [
            format!("ada@example.com {}", carol.user_id),
            "dan@example.com -".to_owned(),
        ]
    );
}

#[tokio::test]
async fn invitations_004_an_invitation_whose_inviter_s_account_is_deleted_lists_no_inviter() {
    let Shared {
        api,
        ada,
        workspace,
        ..
    } = shared().await;
    let dan = users::register(&api, "dan@example.com").await;
    let invited = invitations::invite(&api, &ada, &workspace, "dan@example.com", "member").await;
    assert_eq!(invited.status, 201);

    let deleted = delete(&api, &ada).await;
    let listed = api
        .get("/v1/users/me/invitations")
        .bearer(&dan.access)
        .send()
        .await;

    assert_eq!(deleted.status, 204);
    assert_eq!(listed.status, 200);
    let items = listed.json()["items"].clone();
    assert_eq!(items.as_array().map(Vec::len), Some(1));
    assert_eq!(items[0]["workspace_id"], json!(workspace));
    assert_eq!(items[0]["inviter_name"], json!(null));
}
// end part: invitations
