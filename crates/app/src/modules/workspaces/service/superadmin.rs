//! What a superadmin does with the deployment's workspaces: listing every one, creating one,
//! and deleting any (the workspaces spec's "A superadmin's routes").

use std::collections::HashMap;

use platform::{
    authz::Superadmin,
    db::UnitOfWork,
    idempotency::{Claimed, IdempotencyKey},
    lists::{List, Page, Search, key},
    types::{UserId, WorkspaceId},
};

use super::{Founder, erase, found};
use crate::modules::{
    access::{self, Census},
    accounts::{self, PublicProfile},
    audit::{self as log, Access, Entry},
    auth,
    workspaces::{
        audit::{self, Acted},
        error::WorkspacesError,
        events as published,
        model::{Current, WorkspaceName, WorkspaceOverview, WorkspaceOwner},
        repo,
    },
};

/// What an entry that a superadmin's route records says of how the caller acted: as a
/// superadmin, as a refusal of such a route says too (SUPERADMINS-004), and not as a member of
/// the workspace.
const AS_SUPERADMIN: Acted = Acted {
    access: Some(Access::Superadmin),
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

/// Creates a workspace named `name` that the caller owns, for a superadmin, in the unit of work
/// that their proof opened, which claims `key` first, so a retry with the same key and request
/// answers the same workspace: the workspace, the owner's membership, the entry in the
/// workspace's log, the entry in the superadmin's own, and the event that tells the caller's
/// streams, with the session that created it, commit together (WORKSPACES-031, PLATFORM-036).
pub(in crate::modules::workspaces) async fn create(
    mut uow: UnitOfWork,
    superadmin: &Superadmin,
    key: Option<&IdempotencyKey>,
    name: WorkspaceName,
) -> Result<Current, WorkspacesError> {
    let caller = superadmin.principal();
    if let Claimed::Replay(created) = uow.claim(caller.user_id(), key).await? {
        return Ok(created);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "a superadmin who creates a workspace is recorded as its owner (the workspaces spec)"
    )]
    let founder = Founder {
        owner: caller.user_id(),
        scope: |workspace| superadmin.owner_scope(workspace),
    };
    let created = found(&mut uow, &name, founder).await?;
    let workspace = created.workspace.id;
    // The superadmin's own log holds the creation too, since the workspace's goes with it.
    let entry = Entry::detailed(
        audit::WORKSPACE_CREATED,
        audit::WORKSPACE,
        workspace.uuid(),
        AS_SUPERADMIN,
    );
    log::record(&mut uow, caller.audit_scope(), entry).await?;
    let payload = published::MembershipCreated {
        workspace_id: workspace,
        session_id: caller.session_id(),
    };
    uow.publish_to_user(&published::MEMBERSHIP_CREATED, caller.user_id(), &payload)
        .await?;
    uow.commit_with(&created).await?;
    Ok(created)
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
    let entry = Entry::detailed(
        audit::WORKSPACE_DELETED,
        audit::WORKSPACE,
        workspace.uuid(),
        AS_SUPERADMIN,
    );
    log::record(&mut uow, caller.audit_scope(), entry).await?;
    if !access::dissolve_any(&mut uow, superadmin, workspace).await? {
        // No workspace has the ID: the unit of work rolls back, and the entry with it.
        return Err(WorkspacesError::NotFound);
    }
    erase(&mut uow, workspace).await?;
    Ok(uow.commit().await?)
}
