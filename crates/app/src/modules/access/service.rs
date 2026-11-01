//! What other modules ask of `access`, inside their unit of work, and the `AccessPolicy` that
//! `platform::authz` asks on every workspace request (ACCESS-003, the access spec's "Policy").

use platform::{
    authn::{BoxFuture, Principal},
    authz::{AccessPolicy, Authorized, Decision},
    db::UnitOfWork,
    errors::Internal,
    lists::{List, Page, key},
    types::{Timestamp, UserId, WorkspaceId},
};

use super::{
    error::AccessError,
    model::{MemberFilter, Membership, Role},
    permissions::{self, MembersRead, MembersRemove},
    repo,
};

/// Decides by the caller's membership and the matrix.
pub(crate) struct Policy;

impl AccessPolicy for Policy {
    fn authorize<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        user: UserId,
        workspace: WorkspaceId,
        permission: &'static str,
    ) -> BoxFuture<'a, Result<Decision, Internal>> {
        Box::pin(async move {
            Ok(match repo::role_held(uow, workspace, user).await? {
                None => Decision::NotMember,
                Some(role) if permissions::grants(role, permission) => {
                    Decision::Allowed(role.name())
                }
                Some(_) => Decision::Denied,
            })
        })
    }
}

/// What removing a member came to (ACCESS-004).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    /// The membership is gone.
    Removed,
    /// The user isn't a member of the workspace.
    NotMember,
    /// The member ranks the same as the caller, or higher, so nothing changed: the caller refuses
    /// the operation through `authz::refuse` (PLATFORM-025).
    Outranked,
}

/// Makes `user` the owner of the new `workspace`, and returns when they joined (ACCESS-001).
/// Only the operation that creates a workspace calls it.
pub(crate) async fn add_owner(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<Timestamp, AccessError> {
    Ok(repo::insert(uow, workspace, user, Role::Owner).await?)
}

/// Removes `user` from the caller's workspace, if the caller outranks them (ACCESS-004). The
/// delete itself checks the rank, so it locks the member's row only when it may remove them, and
/// no two removals wait on each other's rows: no two members each outrank the other (DATA-25).
pub(crate) async fn remove(
    uow: &mut UnitOfWork,
    authorized: &Authorized<MembersRemove>,
    user: UserId,
) -> Result<Removal, AccessError> {
    let caller = Role::from_name(authorized.role())?;
    let workspace = authorized.workspace_id();
    let below: Vec<Role> = Role::ALL
        .into_iter()
        .filter(|role| caller.outranks(*role))
        .collect();
    if repo::delete_at(uow, workspace, user, &below).await? {
        return Ok(Removal::Removed);
    }
    Ok(match repo::role(uow, workspace, user).await? {
        None => Removal::NotMember,
        Some(_) => Removal::Outranked,
    })
}

/// A page of the caller's workspace's members, the latest joined first, kept to `filter`.
pub(crate) async fn members(
    uow: &mut UnitOfWork,
    authorized: &Authorized<MembersRead>,
    filter: &MemberFilter,
    page: &Page,
) -> Result<List<Membership>, AccessError> {
    let rows = repo::members(
        uow,
        authorized.workspace_id(),
        filter.role,
        filter.users.as_deref(),
        page,
    )
    .await?;
    Ok(page.list(rows, |member| key(member.joined_at, member.user_id.uuid())))
}

/// The user IDs of every member of the caller's workspace, which a search of the members hands
/// `accounts` (the access spec's "Searching members").
pub(crate) async fn member_ids(
    uow: &mut UnitOfWork,
    authorized: &Authorized<MembersRead>,
) -> Result<Vec<UserId>, AccessError> {
    Ok(repo::member_ids(uow, authorized.workspace_id()).await?)
}

/// A page of the caller's own memberships, the latest joined first, among `workspaces` only when
/// a search named them.
pub(crate) async fn joined(
    uow: &mut UnitOfWork,
    principal: &Principal,
    workspaces: Option<&[WorkspaceId]>,
    page: &Page,
) -> Result<List<Membership>, AccessError> {
    let rows = repo::joined(uow, principal.user_id(), workspaces, page).await?;
    Ok(page.list(rows, |member| {
        key(member.joined_at, member.workspace_id.uuid())
    }))
}

/// Every workspace the caller belongs to, with their role in each and when they joined, and
/// nothing else (ACCESS-005).
pub(crate) async fn memberships(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<Vec<Membership>, AccessError> {
    Ok(repo::memberships(uow, principal.user_id()).await?)
}
