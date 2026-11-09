//! What other modules ask of `access`, inside their unit of work, and the `AccessPolicy` that
//! `platform::authz` asks on every workspace request (ACCESS-003, the access spec's "Policy").

use std::collections::HashMap;

use platform::{
    authn::{BoxFuture, Principal},
    authz::{AccessPolicy, Authorized, Decision, Memberships},
    db::UnitOfWork,
    errors::Internal,
    events::{self, AccessChange},
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

    fn memberships<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        users: &'a [UserId],
    ) -> BoxFuture<'a, Result<HashMap<UserId, Memberships>, Internal>> {
        Box::pin(async move {
            let mut memberships: HashMap<UserId, Memberships> = HashMap::new();
            for (user, workspace, role, joined) in repo::memberships_of(uow, users).await? {
                memberships.entry(user).or_default().insert(
                    workspace,
                    joined,
                    &permissions::held(role),
                );
            }
            Ok(memberships)
        })
    }
}

/// Makes the unit of work that adds a membership commit in the order of the commits that
/// publish events, so each event tells whether the membership had committed before it (SEC-19).
fn ordered(uow: &mut UnitOfWork) {
    uow.order_with_events();
}

/// Signals that `user`'s membership in `workspace` started or ended, so that, once the unit of
/// work commits, the hub closes the streams of theirs that PLATFORM-031 says a membership change
/// closes (ACCESS-006, D35).
async fn changed(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<(), Internal> {
    events::signal(
        uow,
        AccessChange::MembershipChanged {
            user_id: user,
            workspace_id: workspace,
        },
    )
    .await
}

/// What removing a member came to (ACCESS-004).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    /// The membership is gone.
    Removed,
    /// The user isn't a member of the workspace.
    NotMember,
    /// The member ranks the same as the caller, or higher, so nothing changed: the caller refuses
    /// the operation through `authz::refuse` (PLATFORM-010).
    Outranked,
}

/// Makes `user` the owner of the new `workspace`, and returns when they joined (ACCESS-001).
/// Only the operation that creates a workspace calls it.
pub(crate) async fn add_owner(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<Timestamp, AccessError> {
    let joined_at = repo::insert(uow, workspace, user, Role::Owner).await?;
    ordered(uow);
    changed(uow, workspace, user).await?;
    Ok(joined_at)
}

// part: invitations
/// Locks the membership of `workspace`'s owner `FOR KEY SHARE`, and answers whether the workspace
/// is still there: every workspace has an owner (ACCESS-001), so one without went with its
/// owner's account while the lock waited for that deletion (ACCESS-008). Accepting an invitation
/// calls it first, before it locks the invitation, as a deletion of the owner's account locks the
/// owner's membership before the invitations its workspace's cascade deletes (DATA-20).
pub(crate) async fn hold_owner(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
) -> Result<bool, AccessError> {
    Ok(repo::hold_owner(uow, workspace).await?)
}

/// Makes `user` a member of `workspace` at `role`, and returns when they joined, or `None` when
/// they already are one, which changes nothing (D16). Only accepting an invitation calls it, once
/// [`hold_owner`] has locked the owner's membership.
pub(crate) async fn add_member(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
    role: Role,
) -> Result<Option<Timestamp>, AccessError> {
    let joined_at = repo::insert_unless_member(uow, workspace, user, role).await?;
    if joined_at.is_some() {
        ordered(uow);
        changed(uow, workspace, user).await?;
    }
    Ok(joined_at)
}

/// Whether `user` belongs to `workspace`.
pub(crate) async fn is_member(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<bool, AccessError> {
    Ok(repo::role(uow, workspace, user).await?.is_some())
}
// end part: invitations

/// Locks every membership of the caller, and answers whether their account may be deleted: that
/// no workspace they own has another member (ACCESS-008, ACCOUNTS-012). Only the deletion of the
/// caller's account calls it, once it holds the user's row, so it takes its locks before any other
/// (the accounts spec's "Deleting an account").
pub(crate) async fn erasable(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<bool, AccessError> {
    let user = principal.user_id();
    let owned: Vec<WorkspaceId> = repo::held(uow, user)
        .await?
        .into_iter()
        .filter(|(_, role)| *role == Role::Owner)
        .map(|(workspace, _)| workspace)
        .collect();
    Ok(owned.is_empty() || !repo::others_in(uow, &owned, user).await?)
}

/// Every membership of the caller, with its workspace and role, which the personal data contract
/// of `workspaces` reads once [`erasable`] has locked them, so its lock holds them already.
pub(crate) async fn held(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<Vec<(WorkspaceId, Role)>, AccessError> {
    Ok(repo::held(uow, principal.user_id()).await?)
}

/// Ends the caller's membership in `workspace`, with its signal (ACCESS-006), as their account is
/// deleted: the personal data contract of `workspaces` calls it for each workspace they belong
/// to, once [`erasable`] has locked their memberships.
pub(crate) async fn leave(
    uow: &mut UnitOfWork,
    principal: &Principal,
    workspace: WorkspaceId,
) -> Result<(), AccessError> {
    let user = principal.user_id();
    repo::delete(uow, workspace, user).await?;
    Ok(changed(uow, workspace, user).await?)
}

/// Deletes every membership of `user` that remains, signalling each (ACCESS-006): none, once the
/// contract of `workspaces` has had them leave each workspace they belonged to.
pub(super) async fn leave_all(uow: &mut UnitOfWork, user: UserId) -> Result<(), Internal> {
    for workspace in repo::delete_all(uow, user).await? {
        changed(uow, workspace, user).await?;
    }
    Ok(())
}

/// Removes `user` from the caller's workspace, if the caller outranks them (ACCESS-004). The
/// delete itself checks the rank, so it locks the member's row only when it may remove them, and
/// no two removals wait on each other's rows: no two members each outrank the other (DATA-20).
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
        changed(uow, workspace, user).await?;
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
    Ok(page.list(rows, |member| key(member.joined_at, member.id)))
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
    Ok(page.list(rows, |member| key(member.joined_at, member.id)))
}

/// The ID of every workspace the caller belongs to, and nothing else (ACCESS-005).
pub(crate) async fn workspace_ids(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<Vec<WorkspaceId>, AccessError> {
    Ok(repo::workspace_ids(uow, principal.user_id()).await?)
}
