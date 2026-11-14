//! A user creating a workspace, which they then own (the workspaces spec's "Users creating
//! workspaces").

use platform::{
    authn::Principal,
    db::Database,
    idempotency::{Claimed, IdempotencyKey},
    types::Client,
};

use super::{Founder, found};
use crate::modules::workspaces::{
    error::WorkspacesError,
    events as published,
    model::{Current, NewWorkspace},
};

/// Creates a workspace that the caller owns, in a unit of work of its own, which claims `key`
/// first, so a retry with the same key and request answers the same workspace, and tells the
/// caller's streams, with the session that created it (WORKSPACES-001, PLATFORM-036).
pub(crate) async fn create(
    database: &Database,
    client: &Client,
    principal: &Principal,
    key: Option<&IdempotencyKey>,
    input: NewWorkspace,
) -> Result<Current, WorkspacesError> {
    let mut uow = database.begin(client).await?;
    if let Claimed::Replay(created) = uow.claim(principal.user_id(), key).await? {
        return Ok(created);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "a user who creates a workspace is recorded as its founder (the workspaces spec)"
    )]
    let founder = Founder {
        owner: principal.user_id(),
        scope: |workspace| principal.founder_scope(workspace),
    };
    let created = found(&mut uow, &input.name, founder).await?;
    let payload = published::MembershipCreated {
        workspace_id: created.workspace.id,
        session_id: principal.session_id(),
    };
    uow.publish_to_user(
        &published::MEMBERSHIP_CREATED,
        principal.user_id(),
        &payload,
    )
    .await?;
    uow.commit_with(&created).await?;
    Ok(created)
}
