//! The members endpoints: listing a workspace's members, reading one, changing a member's role,
//! removing a member, and leaving (the workspaces spec).

use std::collections::HashMap;

use platform::{
    authn::AuditScope,
    authz::{self, Authorized},
    db::UnitOfWork,
    errors::{Internal, RequestError},
    lists::{List, Page, Search},
    types::{UserId, Version, WorkspaceId},
};

use crate::modules::{
    access::{
        self, MemberFilter, MembersRead, MembersRemove, MembersUpdate, Membership, Removal, Role,
        RoleChange, WorkspaceLeave,
    },
    accounts::{self, PublicProfile},
    audit::{self as log, AuditError, Entry},
    workspaces::{
        audit,
        error::WorkspacesError,
        events as published,
        model::{CurrentMember, Member, NewRole},
    },
};

/// The member whose membership is `membership` and whose profile is `profile`.
fn shown(membership: Membership, profile: PublicProfile) -> Member {
    Member {
        user_id: membership.user_id,
        name: profile.name,
        email: profile.email,
        role: membership.role,
        joined_at: membership.joined_at,
        // part: files
        avatar_url: profile.avatar_url,
        // end part: files
    }
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
    let memberships: List<Membership> = access::members(uow, authorized, &filter, page).await?;
    let ids: Vec<_> = memberships.items.iter().map(|item| item.user_id).collect();
    let mut profiles: HashMap<UserId, PublicProfile> = accounts::profiles(uow, &ids)
        .await?
        .into_iter()
        .map(|profile| (profile.id, profile))
        .collect();
    let items = memberships
        .items
        .into_iter()
        .filter_map(|membership| {
            profiles
                .remove(&membership.user_id)
                .map(|profile| shown(membership, profile))
        })
        .collect();
    Ok(List {
        items,
        next_cursor: memberships.next_cursor,
    })
}

/// The member of the caller's workspace that `user` is, as they now stand, with the version of
/// their membership (WORKSPACES-011).
pub(crate) async fn member(
    uow: &mut UnitOfWork,
    authorized: &Authorized<MembersRead>,
    user: UserId,
) -> Result<CurrentMember, WorkspacesError> {
    let Some((membership, version)) = access::member(uow, authorized, user).await? else {
        return Err(WorkspacesError::MemberNotFound);
    };
    current(uow, membership, version).await
}

/// The member whose membership is `membership`, at `version`, with their profile. A user whose
/// account went between the two reads is no member.
async fn current(
    uow: &mut UnitOfWork,
    membership: Membership,
    version: Version,
) -> Result<CurrentMember, WorkspacesError> {
    let profile = accounts::profiles(uow, &[membership.user_id])
        .await?
        .into_iter()
        .next()
        .ok_or(WorkspacesError::MemberNotFound)?;
    Ok(CurrentMember {
        member: shown(membership, profile),
        version,
    })
}

/// Changes the role of `user` in the caller's workspace to `new`, in the unit of work it was
/// authorized in: whatever the membership's version when `versions` is `None`, and otherwise
/// only if its version is one of them. The role, its audit entry, the members' event and the
/// access-change signal commit together; a role that the member holds already changes nothing
/// (WORKSPACES-012, WORKSPACES-013, PLATFORM-071).
pub(crate) async fn change_role(
    mut uow: UnitOfWork,
    authorized: &Authorized<MembersUpdate>,
    user: UserId,
    new: NewRole,
    versions: Option<&[Version]>,
) -> Result<CurrentMember, WorkspacesError> {
    let (membership, version) =
        match access::change_role(&mut uow, authorized, user, new.role, versions).await? {
            RoleChange::NotMember => return Err(WorkspacesError::MemberNotFound),
            RoleChange::Outranked => return Err(authz::refuse(uow, authorized).await.into()),
            RoleChange::PreconditionFailed => return Err(RequestError::PreconditionFailed.into()),
            RoleChange::Unchanged { member, version } => (member, version),
            RoleChange::Changed {
                from,
                member,
                version,
            } => {
                announce_role(&mut uow, authorized, user, from, new.role).await?;
                (member, version)
            }
        };
    let current = current(&mut uow, membership, version).await?;
    uow.commit().await?;
    Ok(current)
}

/// Records and publishes that `user`'s role in the caller's workspace changed from `from` to `to`
/// (WORKSPACES-012).
async fn announce_role(
    uow: &mut UnitOfWork,
    authorized: &Authorized<MembersUpdate>,
    user: UserId,
    from: Role,
    to: Role,
) -> Result<(), WorkspacesError> {
    let workspace = authorized.workspace_id();
    let mut entry = Entry::new(
        audit::MEMBER_ROLE_CHANGED,
        audit::WORKSPACE,
        workspace.uuid(),
    );
    entry.target = Some(user);
    entry.details = serde_json::json!({ "from": from.name(), "to": to.name() });
    log::record(uow, authorized.audit_scope(), entry).await?;
    let payload = published::MemberRoleChanged {
        workspace_id: workspace,
        user_id: user,
        role: to,
    };
    uow.publish_to_workspace(&published::MEMBER_ROLE_CHANGED, workspace, &payload)
        .await?;
    Ok(())
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
    let mut entry = Entry::new(audit::MEMBER_REMOVED, audit::WORKSPACE, workspace.uuid());
    entry.target = Some(user);
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    let payload = published::MemberRemoved {
        workspace_id: workspace,
        user_id: user,
    };
    uow.publish_to_workspace(&published::MEMBER_REMOVED, workspace, &payload)
        .await?;
    Ok(uow.commit().await?)
}

/// Ends the caller's membership in their workspace, in the unit of work it was authorized in:
/// the membership's end, its audit entry, the members' event and the access-change signal commit
/// together (WORKSPACES-014).
pub(crate) async fn leave(
    mut uow: UnitOfWork,
    authorized: &Authorized<WorkspaceLeave>,
) -> Result<(), WorkspacesError> {
    access::leave(&mut uow, authorized).await?;
    let user = authorized.principal().user_id();
    left(
        &mut uow,
        authorized.audit_scope(),
        authorized.workspace_id(),
        user,
    )
    .await?;
    Ok(uow.commit().await?)
}

/// Records, under `scope`, the log of `workspace` with `user` as the actor, that `user` left it,
/// and publishes it to its members: themselves, or as their account is deleted (WORKSPACES-014,
/// ACCOUNTS-014).
pub(in crate::modules::workspaces) async fn left(
    uow: &mut UnitOfWork,
    scope: AuditScope,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<(), Internal> {
    let mut entry = Entry::new(audit::MEMBER_LEFT, audit::WORKSPACE, workspace.uuid());
    entry.target = Some(user);
    log::record(uow, scope, entry)
        .await
        .map_err(|error| match error {
            AuditError::Internal(error) => error,
        })?;
    let payload = published::MemberLeft {
        workspace_id: workspace,
        user_id: user,
    };
    uow.publish_to_workspace(&published::MEMBER_LEFT, workspace, &payload)
        .await
}
