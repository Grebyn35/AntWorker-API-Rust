//! What other modules ask of `access`, inside their unit of work, and the `AccessPolicy` that
//! `platform::authz` asks on every workspace request (ACCESS-003, the access spec's "Policy").
//! The operations that change a membership's role, or end memberships, are in `changes.rs`.

mod changes;

use std::collections::HashMap;

use platform::{
    authn::{BoxFuture, Principal},
    authz::{AccessPolicy, Authorized, Decision, Memberships},
    db::UnitOfWork,
    errors::Internal,
    events::{self, AccessChange},
    lists::{List, Page, key},
    types::{Timestamp, UserId, Version, WorkspaceId},
};

pub(crate) use self::changes::{
    Removal, RoleChange, Transfer, change_role, dissolve, leave, remove, transfer,
};
use super::{
    error::AccessError,
    model::{MemberFilter, Membership, Role},
    permissions::{self, MembersRead},
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
            // An operation that changes or ends the caller's own membership holds it against
            // every other from here on (ACCESS-009).
            let exclusive = permissions::changes_own_membership(permission);
            Ok(
                match repo::role_held(uow, workspace, user, exclusive).await? {
                    None => Decision::NotMember,
                    Some(role) if permissions::grants(role, permission) => {
                        Decision::Allowed(role.name())
                    }
                    Some(_) => Decision::Denied,
                },
            )
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

/// Signals that `user`'s membership in `workspace` started, ended or changed its role, so that,
/// once the unit of work commits, the hub closes the streams of theirs that PLATFORM-031 says a
/// membership change closes (ACCESS-006, D35).
async fn changed(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<(), Internal> {
    changed_all(uow, workspace, &[user]).await
}

/// Signals, as [`changed`] does for one, that the membership in `workspace` of each of `users`
/// started, ended or changed its role (ACCESS-006).
async fn changed_all(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    users: &[UserId],
) -> Result<(), Internal> {
    let changes: Vec<AccessChange> = users
        .iter()
        .map(|&user_id| AccessChange::MembershipChanged {
            user_id,
            workspace_id: workspace,
        })
        .collect();
    events::signal(uow, &changes).await
}

/// Deletes the membership of `user` in `workspace`, and signals its end (ACCESS-006).
async fn end(uow: &mut UnitOfWork, workspace: WorkspaceId, user: UserId) -> Result<(), Internal> {
    repo::changes::delete(uow, workspace, user).await?;
    changed(uow, workspace, user).await
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
/// is still there: every workspace has an owner (ACCESS-001), so one without went, with its
/// owner's account or by its owner's deletion of it, while the lock waited for that deletion
/// (ACCESS-008, ACCESS-010). Accepting an invitation calls it first, before it locks the
/// invitation, as each of those deletions locks the owner's membership before the invitations
/// that the workspace's cascade deletes (DATA-20).
pub(crate) async fn hold_owner(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
) -> Result<bool, AccessError> {
    loop {
        if repo::hold_owner(uow, workspace).await? {
            return Ok(true);
        }
        // No owner's row was locked: the workspace is gone, or a transfer that the lock waited
        // for made the member it waited at an admin, and the next read locks the new owner's.
        if !repo::has_owner(uow, workspace).await? {
            return Ok(false);
        }
    }
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

/// The workspaces whose only member is `user`, the user whose export the caller's job builds,
/// read by their own memberships, whose content their export holds (ACCOUNTS-024, D40).
pub(crate) async fn sole_workspaces(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<WorkspaceId>, AccessError> {
    Ok(repo::sole(uow, user).await?)
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
pub(crate) async fn end_membership(
    uow: &mut UnitOfWork,
    principal: &Principal,
    workspace: WorkspaceId,
) -> Result<(), AccessError> {
    Ok(end(uow, workspace, principal.user_id()).await?)
}

/// Deletes every membership of `user` that remains, signalling each (ACCESS-006): none, once the
/// contract of `workspaces` has had them leave each workspace they belonged to.
pub(super) async fn leave_all(uow: &mut UnitOfWork, user: UserId) -> Result<(), Internal> {
    for workspace in repo::changes::delete_all(uow, user).await? {
        changed(uow, workspace, user).await?;
    }
    Ok(())
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

/// The membership of `user` in the caller's workspace, if they are a member, with its version,
/// which the member's `ETag` names (WORKSPACES-011).
pub(crate) async fn member(
    uow: &mut UnitOfWork,
    authorized: &Authorized<MembersRead>,
    user: UserId,
) -> Result<Option<(Membership, Version)>, AccessError> {
    Ok(repo::member(uow, authorized.workspace_id(), user).await?)
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

// part: files
/// The ID of every workspace that `user` belongs to, the user whose job the caller runs, and
/// nothing else, as [`workspace_ids`] gives a caller's (ACCESS-005).
pub(crate) async fn workspace_ids_of(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<WorkspaceId>, AccessError> {
    Ok(repo::workspace_ids(uow, user).await?)
}
// end part: files
