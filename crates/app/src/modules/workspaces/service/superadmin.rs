//! What a superadmin does with the deployment's workspaces: listing every one (the workspaces
//! spec's "A superadmin's routes").

use std::collections::HashMap;

use platform::{
    authz::Superadmin,
    db::UnitOfWork,
    lists::{List, Page, Search, key},
    types::UserId,
};

use crate::modules::{
    access,
    accounts::{self, PublicProfile},
    workspaces::{
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
    let rows = repo::superadmin::every(uow, search, page).await?;
    let listed = page.list(rows, |(id, _, created_at)| key(*created_at, id.uuid()));
    let ids: Vec<_> = listed.items.iter().map(|(id, _, _)| *id).collect();
    let mut census = access::census(uow, superadmin, &ids).await?;
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
