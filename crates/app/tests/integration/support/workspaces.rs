//! Builders of workspaces and memberships, through the `owner` connection rather than any part
//! that creates workspaces (TEST-05, the workspaces spec's "Creating a workspace").
// part: removal
//! So the other tests pass in a fork that leaves such a part out.
// end part: removal

use serde_json::{Value, json};

use super::{
    TestDatabase, audit,
    events::{self, Event},
};

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
    audit::in_workspace_log(db, workspace, actor, "workspace.renamed").await;
    db.owner_executes(&events::statement(
        &[Event::to_workspace(
            workspace,
            "workspace.read",
            "note.made",
            "{}",
        )],
        false,
    ))
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
