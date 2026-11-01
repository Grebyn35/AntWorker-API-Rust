//! Builders of workspaces and memberships, through the `owner` connection rather than any part
//! that creates workspaces, so removing a part leaves the other tests passing (TEST-06, the
//! workspaces spec's "Creating a workspace").

use super::TestDatabase;

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

/// Adds `user` to `workspace` at `role`.
pub async fn join(db: &TestDatabase, workspace: &str, user: &str, role: &str) {
    db.owner_executes(&format!(
        "INSERT INTO workspace_memberships (workspace_id, user_id, role) \
         VALUES ('{workspace}', '{user}', '{role}')"
    ))
    .await;
}
