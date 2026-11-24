//! Inviting, as a member's request does, accepting and declining, as the invitee's requests do,
//! and an invitation's row, through the `owner` connection (TEST-09).

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

/// Accepts the invitation `id`, as `user`.
pub async fn accept(api: &Api, user: &SignedIn, id: &str) -> Response {
    api.request(
        http::Method::POST,
        &format!("/v1/users/me/invitations/{id}/accept"),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// Declines the invitation `id`, as `user`.
pub async fn decline(api: &Api, user: &SignedIn, id: &str) -> Response {
    api.request(
        http::Method::POST,
        &format!("/v1/users/me/invitations/{id}/decline"),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// The acceptance of the invitation `id`, as [`accept`] sends it, in a task of its own, which a
/// test holds at a lock.
pub fn accepting(api: &Api, user: &SignedIn, id: &str) -> tokio::task::JoinHandle<Response> {
    let (api, user, id) = (api.clone(), user.clone(), id.to_owned());
    tokio::spawn(async move { accept(&api, &user, &id).await })
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
