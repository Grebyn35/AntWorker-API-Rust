//! What `access` does for a superadmin, whose proof names no workspace: the census of the
//! deployment's workspaces, which their list reads (WORKSPACES-029), and the end of every
//! membership of the workspace that they delete (ACCESS-011).

use std::collections::HashMap;

use platform::{
    authz::Superadmin,
    db::UnitOfWork,
    types::{UserId, WorkspaceId},
};

use super::changes::end_all;
use crate::modules::access::{
    error::AccessError,
    repo::{self, superadmin::Held},
};

/// Who owns a workspace, and how many members it has, as a superadmin sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Census {
    /// The workspace's owner (ACCESS-001).
    pub(crate) owner: UserId,
    /// How many members the workspace has, its owner among them.
    pub(crate) members: i64,
}

/// The owner and the number of members of each of `workspaces`, which the superadmin's list
/// read, for a superadmin, whose proof names no workspace (WORKSPACES-029, DATA-16). A
/// workspace that a deletion took since is left out.
pub(crate) async fn census(
    uow: &mut UnitOfWork,
    superadmin: &Superadmin,
    workspaces: &[WorkspaceId],
) -> Result<HashMap<WorkspaceId, Census>, AccessError> {
    let rows = repo::superadmin::census(uow, superadmin, workspaces).await?;
    Ok(rows
        .into_iter()
        .map(|(workspace, owner, members)| (workspace, Census { owner, members }))
        .collect())
}

/// Ends every membership of `workspace`, the workspace that the path of a superadmin's deletion
/// names, each with its signal (ACCESS-006), before `workspaces` deletes it, and returns whether
/// the workspace was there. It locks the owner's membership first, whoever owns the workspace
/// once the lock is taken, as the owner's own deletion holds it from its authorization on, so it
/// takes turns with whatever else holds it: a transfer, the owner's deletion of the workspace,
/// or the deletion of the owner's account (ACCESS-011, DATA-20).
pub(crate) async fn dissolve_any(
    uow: &mut UnitOfWork,
    superadmin: &Superadmin,
    workspace: WorkspaceId,
) -> Result<bool, AccessError> {
    loop {
        match repo::superadmin::hold_owner(uow, superadmin, workspace).await? {
            Held::Locked => break,
            Held::Moved => {}
            Held::Gone => return Ok(false),
        }
    }
    end_all(uow, workspace).await?;
    Ok(true)
}
