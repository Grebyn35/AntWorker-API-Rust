//! Builders of workspaces and memberships, through the `owner` connection rather than any part
//! that creates workspaces (TEST-05, the workspaces spec's "Creating a workspace").
// part: removal
//! So the other tests pass in a fork that leaves such a part out.
// end part: removal
//!
//! It also sends a workspace's rename, its read and its transfer, requests that more than one test
//! file sends (TEST-09).

use serde_json::{Value, json};

use super::{
    Api, TestDatabase,
    api::Response,
    audit,
    events::{self, Event},
    users::{self, SignedIn},
};

/// Renames `workspace` to `name` as `user`, with `headers`, such as an `If-Match`.
pub async fn rename(
    api: &Api,
    user: &SignedIn,
    workspace: &str,
    name: &str,
    headers: &[(&str, &str)],
) -> Response {
    let mut call = api
        .patch(
            &format!("/v1/workspaces/{workspace}"),
            &json!({ "name": name }),
        )
        .bearer(&user.access);
    for (header, value) in headers {
        call = call.header(header, value);
    }
    call.send().await
}

/// Reads `workspace` as `user`.
pub async fn read(api: &Api, user: &SignedIn, workspace: &str) -> Response {
    api.get(&format!("/v1/workspaces/{workspace}"))
        .bearer(&user.access)
        .send()
        .await
}

/// Transfers `workspace` to `user`, as `caller`.
pub async fn transfer(api: &Api, caller: &SignedIn, workspace: &str, user: &str) -> Response {
    api.post(
        &format!("/v1/workspaces/{workspace}/transfer"),
        &json!({ "user_id": user }),
    )
    .bearer(&caller.access)
    .send()
    .await
}

/// A workspace named Acme that ada@example.com owns and grace@example.com belongs to at `role`,
/// both registered and signed in: the workspace's ID, Ada and Grace.
pub async fn acme(api: &Api, role: &str) -> (String, SignedIn, SignedIn) {
    let ada = users::register(api, "ada@example.com").await;
    let grace = users::register(api, "grace@example.com").await;
    let db = api.database();
    let workspace = arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    join(db, &workspace, &grace.user_id, role).await;
    (workspace, ada, grace)
}

/// Creates a workspace named `name`, with `members` given as user IDs and roles, and returns its
/// ID.
pub async fn arrange(db: &TestDatabase, name: &str, members: &[(&str, &str)]) -> String {
    let name = name.replace('\'', "''");
    let id = db
        .owner_reads(&format!(
            "INSERT INTO workspaces (name) VALUES ('{name}') RETURNING id::text"
        ))
        .await
        .remove(0);
    for (user, role) in members {
        join(db, &id, user, role).await;
    }
    id
}

/// Creates `count` workspaces, named `W1` and on, with `user` a member of each at `role`, in one
/// statement.
pub async fn member_of_many(db: &TestDatabase, user: &str, count: u32, role: &str) {
    db.owner_executes(&format!(
        "WITH created AS (INSERT INTO workspaces (name) \
         SELECT 'W' || n FROM generate_series(1, {count}) AS n RETURNING id) \
         INSERT INTO workspace_memberships (workspace_id, user_id, role) \
         SELECT id, '{user}', '{role}' FROM created"
    ))
    .await;
}

/// Adds `user` to `workspace` at `role`.
pub async fn join(db: &TestDatabase, workspace: &str, user: &str, role: &str) {
    db.owner_executes(&format!(
        "INSERT INTO workspace_memberships (workspace_id, user_id, role) \
         VALUES ('{workspace}', '{user}', '{role}')"
    ))
    .await;
}

/// Gives `workspace` what a deletion of it must take with it: an entry in its log, acted by
/// `actor`, an event, and a job enqueued for it and for `actor`.
pub async fn fill(db: &TestDatabase, workspace: &str, actor: &str) {
    audit::in_workspace_log(db, workspace, actor).await;
    db.owner_executes(&events::statement(&[Event::to_workspace(workspace)], false))
        .await;
    db.owner_executes(&format!(
        "INSERT INTO jobs (kind, payload, user_id, workspace_id) \
         VALUES ('tests.kept', '{{}}', '{actor}', '{workspace}')"
    ))
    .await;
}

/// What `workspace` holds: its row, its memberships, the entries of its log, its events and the
/// jobs enqueued for it, as their counts.
pub async fn held_by(db: &TestDatabase, workspace: &str) -> String {
    db.owner_reads(&format!(
        "SELECT (SELECT count(*) FROM workspaces WHERE id = '{workspace}') || ' ' \
         || (SELECT count(*) FROM workspace_memberships WHERE workspace_id = '{workspace}') \
         || ' ' || (SELECT count(*) FROM audit_entries WHERE workspace_id = '{workspace}') \
         || ' ' || (SELECT count(*) FROM events WHERE workspace_id = '{workspace}') \
         || ' ' || (SELECT count(*) FROM jobs WHERE workspace_id = '{workspace}')"
    ))
    .await
    .remove(0)
}

/// The signal of a change to `user`'s membership in `workspace`, which the transaction
/// `transaction` made.
pub fn signal(user: &str, workspace: &str, transaction: u64) -> Value {
    json!({
        "kind": "membership_changed",
        "user_id": user,
        "workspace_id": workspace,
        "transaction": transaction
    })
}
