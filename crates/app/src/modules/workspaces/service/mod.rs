//! Creating workspaces, listing the caller's, reading, renaming, transferring and deleting one
//! (the workspaces spec). The members' operations are in `members.rs`.
// part: files
//! Setting and removing a workspace's picture are in `picture.rs`.
// end part: files
// part: workspace_setup
//! The operator's setup of a workspace is in `setup.rs`.
// end part: workspace_setup

mod members;
// part: files
pub(super) mod picture;
// end part: files
// part: workspace_setup
pub(super) mod setup;
// end part: workspace_setup

use std::collections::HashMap;

use platform::{
    authn::{AuditScope, Principal},
    authz::{Authorized, Permission},
    db::{Database, UnitOfWork},
    errors::{Internal, RequestError},
    idempotency::{Claimed, IdempotencyKey},
    jobs,
    lists::{List, Page, Search},
    types::{Client, SessionId, UserId, Version, WorkspaceId},
};

pub(super) use self::members::left;
pub(crate) use self::members::{change_role, leave, member, members, remove_member};
use super::{
    audit,
    error::WorkspacesError,
    events as published,
    model::{Current, NewWorkspace, Rename, Transfer, Workspace, WorkspaceItem, WorkspaceName},
    repo,
};
use crate::modules::{
    access::{
        self, Membership, Role, WorkspaceDelete, WorkspaceRead, WorkspaceTransfer, WorkspaceUpdate,
    },
    audit::{self as log, Entry},
    auth,
};

/// Creates a workspace that the caller owns, in a unit of work of its own, which claims `key`
/// first, so a retry with the same key and request answers the same workspace (WORKSPACES-001,
/// PLATFORM-036).
pub(crate) async fn create(
    database: &Database,
    client: &Client,
    principal: &Principal,
    key: Option<&IdempotencyKey>,
    input: NewWorkspace,
) -> Result<Current, WorkspacesError> {
    let mut uow = database.begin(client).await?;
    if let Claimed::Replay(created) = uow.claim(principal.user_id(), key).await? {
        return Ok(created);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "a user who creates a workspace is recorded as its founder (the workspaces spec)"
    )]
    let founder = Founder {
        owner: principal.user_id(),
        session: Some(principal.session_id()),
        scope: |workspace| principal.founder_scope(workspace),
    };
    let created = found(&mut uow, &input.name, founder).await?;
    uow.commit_with(&created).await?;
    Ok(created)
}

/// Whom a workspace is created for, and how its creation is recorded (D33).
pub(super) struct Founder<S> {
    /// The user who becomes the workspace's owner.
    pub(super) owner: UserId,
    /// The session whose request creates the workspace, when one does, which the owner's event
    /// names.
    pub(super) session: Option<SessionId>,
    /// The scope of the entry that records the creation, in the log of the workspace, whose ID
    /// the database gives.
    pub(super) scope: S,
}

/// The one operation that creates a workspace, inside the unit of work of whatever asks for it:
/// the workspace named `name`, its owner's membership, its first audit entry and the owner's
/// event, together (the workspaces spec's "Creating a workspace", D33).
pub(super) async fn found<S>(
    uow: &mut UnitOfWork,
    name: &WorkspaceName,
    founder: Founder<S>,
) -> Result<Current, WorkspacesError>
where
    S: FnOnce(WorkspaceId) -> AuditScope,
{
    let (id, created_at, version) = repo::insert(uow, name).await?;
    access::add_owner(uow, id, founder.owner).await?;
    let scope = (founder.scope)(id);
    let entry = Entry::new(audit::WORKSPACE_CREATED, audit::WORKSPACE, id.uuid());
    // part: workspace_setup
    // An entry whose scope names no actor names the owner as its target, so the log says whom
    // the workspace was created for (WORKSPACES-023).
    let entry = Entry {
        target: scope.actor().is_none().then_some(founder.owner),
        ..entry
    };
    // end part: workspace_setup
    log::record(uow, scope, entry).await?;
    let payload = published::MembershipCreated {
        workspace_id: id,
        session_id: founder.session,
    };
    uow.publish_to_user(&published::MEMBERSHIP_CREATED, founder.owner, &payload)
        .await?;
    Ok(Current {
        workspace: Workspace {
            id,
            name: name.clone(),
            role: Role::Owner,
            created_at,
            // part: files
            picture_url: None,
            // end part: files
        },
        version,
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
            let all = access::workspace_ids(&mut uow, principal).await?;
            Some(repo::matching(&mut uow, &all, search).await?)
        }
    };
    let joined: List<Membership> =
        access::joined(&mut uow, principal, only.as_deref(), page).await?;
    let ids: Vec<_> = joined.items.iter().map(|item| item.workspace_id).collect();
    let mut names: HashMap<_, _> = repo::names(&mut uow, &ids).await?.into_iter().collect();
    // part: files
    let mut pictures = picture::urls(&mut uow, &ids).await?;
    // end part: files
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
                    // part: files
                    picture_url: pictures.remove(&membership.workspace_id),
                    // end part: files
                })
        })
        .collect();
    Ok(List {
        items,
        next_cursor: joined.next_cursor,
    })
}

/// The caller's workspace as it now stands, with their role (WORKSPACES-007).
pub(crate) async fn read(
    uow: &mut UnitOfWork,
    authorized: &Authorized<WorkspaceRead>,
) -> Result<Current, WorkspacesError> {
    current(uow, authorized).await
}

/// The workspace that `authorized` names, as it now stands, with the role of the member it
/// proves, and its version, which an operation that changed the workspace answers too
/// (WORKSPACES-007).
pub(super) async fn current<P: Permission>(
    uow: &mut UnitOfWork,
    authorized: &Authorized<P>,
) -> Result<Current, WorkspacesError> {
    let id = authorized.workspace_id();
    let (name, created_at, version) = repo::read(uow, id).await?;
    Ok(Current {
        workspace: Workspace {
            id,
            name,
            role: Role::from_name(authorized.role())?,
            created_at,
            // part: files
            picture_url: picture::url(uow, id).await?,
            // end part: files
        },
        version,
    })
}

/// Publishes that what the members of the workspace `id` read of it changed, to every member,
/// who reads it again (WORKSPACES-008).
pub(super) async fn publish_updated(
    uow: &mut UnitOfWork,
    id: WorkspaceId,
) -> Result<(), WorkspacesError> {
    let payload = published::WorkspaceUpdated { workspace_id: id };
    uow.publish_to_workspace(&published::WORKSPACE_UPDATED, id, &payload)
        .await?;
    Ok(())
}

/// Renames the caller's workspace, in the unit of work it was authorized in: whatever its
/// version when `versions` is `None`, and otherwise only if its version is one of them, so an
/// empty set fails the precondition. The name, its audit entry and the members' event commit
/// together (WORKSPACES-008, PLATFORM-071).
pub(crate) async fn rename(
    mut uow: UnitOfWork,
    authorized: &Authorized<WorkspaceUpdate>,
    rename: Rename,
    versions: Option<&[Version]>,
) -> Result<Current, WorkspacesError> {
    let id = authorized.workspace_id();
    if !repo::rename(&mut uow, id, &rename.name, versions).await? {
        return Err(RequestError::PreconditionFailed.into());
    }
    let entry = Entry::new(audit::WORKSPACE_RENAMED, audit::WORKSPACE, id.uuid());
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    publish_updated(&mut uow, id).await?;
    let current = current(&mut uow, authorized).await?;
    uow.commit().await?;
    Ok(current)
}

// part: invitations
/// The names of the workspaces `ids` names, which came from a query filtered by tenant, such as
/// an invitee's invitations (DATA-16).
pub(crate) async fn names(
    uow: &mut UnitOfWork,
    ids: &[WorkspaceId],
) -> Result<Vec<(WorkspaceId, WorkspaceName)>, WorkspacesError> {
    Ok(repo::names(uow, ids).await?)
}

/// The IDs among `ids`, passed under the same terms as to [`names`], of the workspaces whose name
/// contains `search` (API-20).
pub(crate) async fn matching(
    uow: &mut UnitOfWork,
    ids: &[WorkspaceId],
    search: &Search,
) -> Result<Vec<WorkspaceId>, WorkspacesError> {
    Ok(repo::matching(uow, ids, search).await?)
}
// end part: invitations

/// Makes the member that `transfer` names, an admin, the owner of the caller's workspace, and the
/// caller an admin, in the unit of work it was authorized in, once the caller's session has
/// proved who its user is: the two roles, the audit entry, the members' event and the two
/// access-change signals commit together (WORKSPACES-015, WORKSPACES-016, WORKSPACES-018).
pub(crate) async fn transfer(
    mut uow: UnitOfWork,
    authorized: &Authorized<WorkspaceTransfer>,
    transfer: Transfer,
) -> Result<(), WorkspacesError> {
    auth::require_recent_proof(&mut uow, authorized.principal()).await?;
    match access::transfer(&mut uow, authorized, transfer.to).await? {
        access::Transfer::NotMember => return Err(WorkspacesError::MemberNotFound),
        access::Transfer::NotAdmin => return Err(WorkspacesError::TransferRequiresAdmin),
        access::Transfer::Transferred => {}
    }
    let workspace = authorized.workspace_id();
    let mut entry = Entry::new(
        audit::WORKSPACE_TRANSFERRED,
        audit::WORKSPACE,
        workspace.uuid(),
    );
    entry.target = Some(transfer.to);
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    let payload = published::WorkspaceTransferred {
        workspace_id: workspace,
        owner_id: transfer.to,
        previous_owner_id: authorized.principal().user_id(),
    };
    uow.publish_to_workspace(&published::WORKSPACE_TRANSFERRED, workspace, &payload)
        .await?;
    Ok(uow.commit().await?)
}

/// Deletes the caller's workspace, with everything in it, in the unit of work it was authorized
/// in, once the caller's session has proved who its user is: the entry in the caller's own log,
/// the end of every membership, each with its access-change signal, the workspace and the jobs
/// enqueued for it commit together (WORKSPACES-017, WORKSPACES-018).
pub(crate) async fn delete(
    mut uow: UnitOfWork,
    authorized: &Authorized<WorkspaceDelete>,
) -> Result<(), WorkspacesError> {
    auth::require_recent_proof(&mut uow, authorized.principal()).await?;
    let workspace = authorized.workspace_id();
    // The entry's foreign keys read the caller's row, so it is written before the workspace's
    // row is locked (the workspaces spec's "Deleting a workspace").
    let entry = Entry::new(audit::WORKSPACE_DELETED, audit::WORKSPACE, workspace.uuid());
    log::record(&mut uow, authorized.principal().audit_scope(), entry).await?;
    access::dissolve(&mut uow, authorized).await?;
    erase(&mut uow, workspace).await?;
    Ok(uow.commit().await?)
}

/// Deletes the workspace `id`, whose memberships the unit of work has ended, with everything in
/// it: the rows that name it, through their foreign keys, and then the jobs enqueued for it,
/// which no foreign key reaches (DATA-23).
pub(super) async fn erase(uow: &mut UnitOfWork, id: WorkspaceId) -> Result<(), Internal> {
    repo::delete(uow, id).await?;
    jobs::forget_workspace(uow, id).await
}
