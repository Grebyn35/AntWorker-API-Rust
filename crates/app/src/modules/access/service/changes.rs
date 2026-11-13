//! The operations that change a membership's role or end memberships: removing a member,
//! changing a role, leaving, a transfer, and what a workspace's deletion does to its memberships
//! (the access spec's "Removing a member", "Changing a role" and "Leaving, transferring and
//! dissolving").

use platform::{
    authz::Authorized,
    db::UnitOfWork,
    types::{UserId, Version},
};

use super::{changed, changed_all, end};
use crate::modules::access::{
    error::AccessError,
    model::{Membership, Role},
    permissions::{
        MembersRemove, MembersUpdate, WorkspaceDelete, WorkspaceLeave, WorkspaceTransfer,
    },
    repo,
};

/// The roles that rank below `role`, which a member at `role` may act on (ACCESS-004).
fn below(role: Role) -> Vec<Role> {
    Role::ALL
        .into_iter()
        .filter(|other| role.outranks(*other))
        .collect()
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
    if repo::changes::delete_at(uow, workspace, user, &below(caller)).await? {
        changed(uow, workspace, user).await?;
        return Ok(Removal::Removed);
    }
    Ok(match repo::role(uow, workspace, user).await? {
        None => Removal::NotMember,
        Some(_) => Removal::Outranked,
    })
}

/// What changing a member's role came to (ACCESS-004).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoleChange {
    /// The member holds the role, which they didn't before.
    Changed {
        /// The role they held before.
        from: Role,
        /// Their membership, as it stands.
        member: Membership,
        /// Its new version.
        version: Version,
    },
    /// The member held the role already, so nothing changed.
    Unchanged {
        /// Their membership.
        member: Membership,
        /// Its version.
        version: Version,
    },
    /// The user isn't a member of the workspace.
    NotMember,
    /// The member, or the role, ranks the same as the caller, or higher, so nothing changed: the
    /// caller refuses the operation through `authz::refuse` (PLATFORM-010).
    Outranked,
    /// The membership's version isn't one that the request's `If-Match` names, so nothing
    /// changed (PLATFORM-071).
    PreconditionFailed,
}

/// Makes `role` the role of `user` in the caller's workspace, if the caller outranks both the
/// member and the role (ACCESS-004), and the membership's version is one of `versions`, or
/// whatever its version when `versions` is `None` (DATA-18), and signals the change (ACCESS-006).
/// It locks the member's row only where the caller outranks them, so it never waits on a member
/// it may not change (DATA-20).
pub(crate) async fn change_role(
    uow: &mut UnitOfWork,
    authorized: &Authorized<MembersUpdate>,
    user: UserId,
    role: Role,
    versions: Option<&[Version]>,
) -> Result<RoleChange, AccessError> {
    let caller = Role::from_name(authorized.role())?;
    if !caller.outranks(role) {
        return Ok(RoleChange::Outranked);
    }
    let workspace = authorized.workspace_id();
    let Some((member, version)) =
        repo::changes::hold_at(uow, workspace, user, &below(caller)).await?
    else {
        return Ok(match repo::role(uow, workspace, user).await? {
            None => RoleChange::NotMember,
            Some(_) => RoleChange::Outranked,
        });
    };
    if versions.is_some_and(|versions| !versions.contains(&version)) {
        return Ok(RoleChange::PreconditionFailed);
    }
    if member.role == role {
        return Ok(RoleChange::Unchanged { member, version });
    }
    Ok(
        match repo::changes::set_role(uow, workspace, user, role, versions).await? {
            None => RoleChange::PreconditionFailed,
            Some(version) => {
                changed(uow, workspace, user).await?;
                RoleChange::Changed {
                    from: member.role,
                    member: Membership { role, ..member },
                    version,
                }
            }
        },
    )
}

/// Ends the caller's membership in their workspace, with its signal (ACCESS-006). The proof holds
/// the membership locked, and only a member who isn't the owner holds it (SEC-09).
pub(crate) async fn leave(
    uow: &mut UnitOfWork,
    authorized: &Authorized<WorkspaceLeave>,
) -> Result<(), AccessError> {
    let user = authorized.principal().user_id();
    Ok(end(uow, authorized.workspace_id(), user).await?)
}

/// What transferring a workspace came to (SEC-09).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transfer {
    /// The member is the owner, and the caller an admin.
    Transferred,
    /// The user isn't a member of the workspace.
    NotMember,
    /// The member isn't an admin, so nothing changed.
    NotAdmin,
}

/// Makes `to`, an admin of the caller's workspace, its owner, and the caller, its owner, an
/// admin, and signals both changes (SEC-09, ACCESS-006). The proof holds the caller's membership
/// locked; this locks the other only while it is an admin's, so it never waits on a member it
/// doesn't change (DATA-20).
pub(crate) async fn transfer(
    uow: &mut UnitOfWork,
    authorized: &Authorized<WorkspaceTransfer>,
    to: UserId,
) -> Result<Transfer, AccessError> {
    let workspace = authorized.workspace_id();
    let owner = authorized.principal().user_id();
    if repo::changes::hold_at(uow, workspace, to, &[Role::Admin])
        .await?
        .is_none()
    {
        return Ok(match repo::role(uow, workspace, to).await? {
            None => Transfer::NotMember,
            Some(_) => Transfer::NotAdmin,
        });
    }
    // The index that keeps one owner is checked as each row changes, so the owner's role goes
    // first (ACCESS-001).
    repo::changes::set_role(uow, workspace, owner, Role::Admin, None).await?;
    repo::changes::set_role(uow, workspace, to, Role::Owner, None).await?;
    changed_all(uow, workspace, &[owner, to]).await?;
    Ok(Transfer::Transferred)
}

/// Ends every membership of the caller's workspace, each with its signal (ACCESS-006), before
/// `workspaces` deletes the workspace: from the highest rank to the lowest, a statement for each
/// role, so it reaches a member's row before every row that member's own operation may lock
/// after theirs, and waits for that operation while it holds nothing the operation still needs
/// (DATA-20).
pub(crate) async fn dissolve(
    uow: &mut UnitOfWork,
    authorized: &Authorized<WorkspaceDelete>,
) -> Result<(), AccessError> {
    let workspace = authorized.workspace_id();
    for role in Role::ALL {
        let users = repo::changes::delete_role(uow, workspace, role).await?;
        changed_all(uow, workspace, &users).await?;
    }
    Ok(())
}
