//! Creating workspaces, listing the caller's, and the members endpoints (the workspaces spec).

use std::collections::HashMap;

use platform::{
    authn::Principal,
    authz::{self, Authorized},
    db::{Database, UnitOfWork},
    events::{self, AccessChange},
    idempotency::{Claimed, IdempotencyKey},
    lists::{List, Page, Search},
    types::{Client, UserId},
};
use serde_json::json;

use super::{
    audit,
    error::WorkspacesError,
    events as published,
    model::{Created, Member, NewWorkspace, WorkspaceItem, WorkspaceName},
    repo,
};
use crate::modules::{
    access::{self, MemberFilter, MembersRead, MembersRemove, Removal, Role},
    accounts,
    audit::{self as log, Entry},
};

/// Creates a workspace that the caller owns, in a unit of work of its own, which claims `key`
/// first, so a retry with the same key and request answers the same workspace (WORKSPACES-001,
/// PLATFORM-018).
pub(crate) async fn create(
    database: &Database,
    client: &Client,
    principal: &Principal,
    key: Option<&IdempotencyKey>,
    input: NewWorkspace,
) -> Result<Created, WorkspacesError> {
    let mut uow = database.begin(client).await?;
    if let Claimed::Replay(created) = uow.claim(principal.user_id(), key).await? {
        return Ok(created);
    }
    let created = found(&mut uow, principal, &input.name).await?;
    uow.commit_with(&created).await?;
    Ok(created)
}

/// The one operation that creates a workspace, inside the unit of work of whatever asks for it:
/// the workspace, its owner's membership, its first audit entry and the owner's event, together
/// (the workspaces spec's "Creating a workspace", D33).
async fn found(
    uow: &mut UnitOfWork,
    principal: &Principal,
    name: &WorkspaceName,
) -> Result<Created, WorkspacesError> {
    let (id, created_at) = repo::insert(uow, name).await?;
    access::add_owner(uow, id, principal.user_id()).await?;
    #[expect(
        clippy::disallowed_methods,
        reason = "this is the operation that creates a workspace (the workspaces spec)"
    )]
    let scope = principal.founder_scope(id);
    let entry = Entry::new(audit::WORKSPACE_CREATED, audit::WORKSPACE, id.uuid());
    log::record(uow, scope, entry).await?;
    // The session that caused the event, so its own device can tell it already knows.
    let payload = json!({ "workspace_id": id, "session_id": principal.session_id() });
    uow.publish_to_user(
        &published::MEMBERSHIP_CREATED,
        principal.user_id(),
        &payload,
    )
    .await?;
    Ok(Created {
        id,
        name: name.clone(),
        role: Role::Owner,
        created_at,
    })
}

/// A page of the workspaces the caller belongs to, the latest joined first, kept to those whose
/// name contains `search` (WORKSPACES-003).
pub(crate) async fn list(
    database: &Database,
    client: &Client,
    principal: &Principal,
    search: Option<&Search>,
    page: &Page,
) -> Result<List<WorkspaceItem>, WorkspacesError> {
    let mut uow = database.begin(client).await?;
    let only = match search {
        None => None,
        Some(search) => {
            let all: Vec<_> = access::memberships(&mut uow, principal)
                .await?
                .into_iter()
                .map(|membership| membership.workspace_id)
                .collect();
            Some(repo::matching(&mut uow, &all, search).await?)
        }
    };
    let joined = access::joined(&mut uow, principal, only.as_deref(), page).await?;
    let ids: Vec<_> = joined.items.iter().map(|item| item.workspace_id).collect();
    let mut names: HashMap<_, _> = repo::names(&mut uow, &ids).await?.into_iter().collect();
    // A workspace deleted between the two reads drops out of the page.
    let items = joined
        .items
        .into_iter()
        .filter_map(|membership| {
            names
                .remove(&membership.workspace_id)
                .map(|name| WorkspaceItem {
                    id: membership.workspace_id,
                    name,
                    role: membership.role,
                    joined_at: membership.joined_at,
                })
        })
        .collect();
    Ok(List {
        items,
        next_cursor: joined.next_cursor,
    })
}

/// A page of the caller's workspace's members, the latest joined first, of `role` only, and kept
/// to those whose name or email contains `search` (WORKSPACES-004).
pub(crate) async fn members(
    uow: &mut UnitOfWork,
    authorized: &Authorized<MembersRead>,
    role: Option<Role>,
    search: Option<&Search>,
    page: &Page,
) -> Result<List<Member>, WorkspacesError> {
    let users = match search {
        None => None,
        Some(search) => {
            let all = access::member_ids(uow, authorized).await?;
            Some(accounts::matching(uow, &all, search).await?)
        }
    };
    let filter = MemberFilter { role, users };
    let memberships = access::members(uow, authorized, &filter, page).await?;
    let ids: Vec<_> = memberships.items.iter().map(|item| item.user_id).collect();
    let mut profiles: HashMap<_, _> = accounts::profiles(uow, &ids)
        .await?
        .into_iter()
        .map(|profile| (profile.id, profile))
        .collect();
    let items = memberships
        .items
        .into_iter()
        .filter_map(|membership| {
            profiles.remove(&membership.user_id).map(|profile| Member {
                user_id: membership.user_id,
                name: profile.name,
                email: profile.email,
                role: membership.role,
                joined_at: membership.joined_at,
            })
        })
        .collect();
    Ok(List {
        items,
        next_cursor: memberships.next_cursor,
    })
}

/// Removes `user` from the caller's workspace, in the unit of work it was authorized in: the
/// membership, its audit entry, the members' event and the access-change signal commit together
/// (WORKSPACES-005, WORKSPACES-006).
pub(crate) async fn remove_member(
    mut uow: UnitOfWork,
    authorized: &Authorized<MembersRemove>,
    user: UserId,
) -> Result<(), WorkspacesError> {
    if user == authorized.principal().user_id() {
        return Err(WorkspacesError::CannotRemoveSelf);
    }
    match access::remove(&mut uow, authorized, user).await? {
        Removal::NotMember => return Err(WorkspacesError::MemberNotFound),
        Removal::Outranked => return Err(authz::refuse(uow, authorized).await.into()),
        Removal::Removed => {}
    }
    let workspace = authorized.workspace_id();
    let mut entry = Entry::new(audit::MEMBER_REMOVED, audit::MEMBER, user.uuid());
    entry.target = Some(user);
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    let payload = json!({ "workspace_id": workspace, "user_id": user });
    uow.publish_to_workspace(&published::MEMBER_REMOVED, workspace, &payload)
        .await?;
    events::signal(
        &mut uow,
        AccessChange::MembershipEnded {
            user_id: user,
            workspace_id: workspace,
        },
    )
    .await?;
    Ok(uow.commit().await?)
}
