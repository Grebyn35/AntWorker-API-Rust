//! What a superadmin does with the deployment's workspaces: listing every one, and deleting any
//! (the workspaces spec's "A superadmin's routes").

use std::collections::HashMap;

use platform::{
    authz::Superadmin,
    db::UnitOfWork,
    lists::{List, Page, Search, key},
    types::{UserId, WorkspaceId},
};

use super::erase;
use crate::modules::{
    access::{self, Census},
    accounts::{self, PublicProfile},
    audit::{self as log, Entry},
    auth,
    workspaces::{
        audit,
        error::WorkspacesError,
        model::{WorkspaceOverview, WorkspaceOwner},
        repo,
    },
};

/// A page of every workspace of the deployment, each with its owner and how many members it
/// has, the latest created first, kept to those whose name contains `search`, for a superadmin,
/// whose proof names no workspace (WORKSPACES-029, DATA-16). A workspace that a deletion takes
/// between the reads drops out of the page.
pub(in crate::modules::workspaces) async fn every(
    uow: &mut UnitOfWork,
    superadmin: &Superadmin,
    search: Option<&Search>,
    page: &Page,
) -> Result<List<WorkspaceOverview>, WorkspacesError> {
    let rows = repo::superadmin::every(uow, superadmin, search, page).await?;
    let listed = page.list(rows, |(id, _, created_at)| key(*created_at, id.uuid()));
    let ids: Vec<_> = listed.items.iter().map(|(id, _, _)| *id).collect();
    let mut census: HashMap<WorkspaceId, Census> = access::census(uow, superadmin, &ids).await?;
    let owners: Vec<UserId> = census.values().map(|counted| counted.owner).collect();
    let profiles: HashMap<UserId, PublicProfile> = accounts::profiles(uow, &owners)
        .await?
        .into_iter()
        .map(|profile| (profile.id, profile))
        .collect();
    let items = listed
        .items
        .into_iter()
        .filter_map(|(id, name, created_at)| {
            let counted = census.remove(&id)?;
            let owner = profiles.get(&counted.owner)?;
            Some(WorkspaceOverview {
                id,
                name,
                created_at,
                member_count: counted.members,
                owner: WorkspaceOwner {
                    user_id: owner.id,
                    name: owner.name.clone(),
                    email: owner.email.clone(),
                },
            })
        })
        .collect();
    Ok(List {
        items,
        next_cursor: listed.next_cursor,
    })
}

/// Deletes `workspace`, the workspace that the path names, whoever its members are, with
/// everything in it, for a superadmin, in the unit of work that their proof opened, once their
/// session has proved who its user is: the entry in the superadmin's own log, the end of every
/// membership, each with its access-change signal, the workspace and the jobs enqueued for it
/// commit together. `workspace` is `None` when the path's ID isn't a UUID, which names no
/// workspace, as an ID that no workspace has names none (WORKSPACES-030, PLATFORM-072).
pub(in crate::modules::workspaces) async fn delete(
    mut uow: UnitOfWork,
    superadmin: &Superadmin,
    workspace: Option<WorkspaceId>,
) -> Result<(), WorkspacesError> {
    let caller = superadmin.principal();
    auth::require_recent_proof(&mut uow, caller).await?;
    let workspace = workspace.ok_or(WorkspacesError::NotFound)?;
    // The entry's foreign keys read the caller's row, so it is written before any row of the
    // workspace is locked (the workspaces spec's "Deleting a workspace"). It says that the
    // caller deleted the workspace as a superadmin, not as its owner.
    let mut entry = Entry::new(audit::WORKSPACE_DELETED, audit::WORKSPACE, workspace.uuid());
    entry.details = serde_json::json!({ "access": "superadmin" });
    log::record(&mut uow, caller.audit_scope(), entry).await?;
    if !access::dissolve_any(&mut uow, superadmin, workspace).await? {
        // No workspace has the ID: the unit of work rolls back, and the entry with it.
        return Err(WorkspacesError::NotFound);
    }
    erase(&mut uow, workspace).await?;
    Ok(uow.commit().await?)
}
