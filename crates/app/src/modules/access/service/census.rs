//! The census of a deployment's workspaces, which a superadmin reads (WORKSPACES-029).

use std::collections::HashMap;

use platform::{
    authz::Superadmin,
    db::UnitOfWork,
    types::{UserId, WorkspaceId},
};

use crate::modules::access::{error::AccessError, repo};

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
    let rows = repo::census::census(uow, superadmin, workspaces).await?;
    Ok(rows
        .into_iter()
        .map(|(workspace, owner, members)| (workspace, Census { owner, members }))
        .collect())
}
