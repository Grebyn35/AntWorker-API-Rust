//! Inviting, as a member's request does, for the tests of invitations and of the emails they
//! send, and an invitation's row, through the `owner` connection (TEST-09).

use serde_json::json;

use super::{Api, TestDatabase, api::Response, users::SignedIn};

/// Invites `email` to `workspace` at `role`, as `user`.
pub async fn invite(
    api: &Api,
    user: &SignedIn,
    workspace: &str,
    email: &str,
    role: &str,
) -> Response {
    api.post(
        &format!("/v1/workspaces/{workspace}/invitations"),
        &json!({ "email": email, "role": role }),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// Creates an invitation of `workspace` to `arranged@example.com` at the member role, sent by
/// nobody, lasting 7 days, and returns its ID.
pub async fn arrange(db: &TestDatabase, workspace: &str) -> String {
    db.owner_reads(&format!(
        "INSERT INTO invitations (workspace_id, email, role, expires_at) \
         VALUES ('{workspace}', 'arranged@example.com', 'member', now() + interval '7 days') \
         RETURNING id::text"
    ))
    .await
    .remove(0)
}
