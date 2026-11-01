//! Inviting, as a member's request does, for the tests of invitations and of the emails they
//! send.

use serde_json::json;

use super::{Api, api::Response, users::SignedIn};

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
