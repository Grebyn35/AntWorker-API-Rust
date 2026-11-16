//! What a workspace's members do with its invitations: reading and listing them, and changing,
//! sending again and cancelling one (the invitations spec's "Reading and listing a workspace's
//! invitations" and "Changing, sending again and cancelling").

use platform::{
    authz::{self, Authorize, Authorized, Permission},
    db::UnitOfWork,
    errors::RequestError,
    lists::{List, Page, Search, key},
    rate_limit::RateLimits,
    types::Version,
};
use serde::Serialize;

use super::{Invitations, announce, send_email, tell};
use crate::modules::{
    access::{
        self, InvitationsCancel, InvitationsCreate, InvitationsRead, InvitationsUpdate, Role,
    },
    accounts,
    audit::{self as log, Entry},
    invitations::{
        audit,
        error::InvitationsError,
        events as published,
        model::{CurrentInvitation, Invitation, NewRole},
        repo::managing as repo,
    },
    workspaces::ChangedRole,
};

/// The invitation `id` of the caller's workspace, expired or not, with its version
/// (INVITATIONS-011).
pub(crate) async fn invitation(
    uow: &mut UnitOfWork,
    authorized: &Authorized<InvitationsRead>,
    id: uuid::Uuid,
) -> Result<CurrentInvitation, InvitationsError> {
    repo::read(uow, authorized.workspace_id(), id)
        .await?
        .ok_or(InvitationsError::NotFound)
}

/// A page of the invitations of the caller's workspace, expired or not, the latest first, of
/// those whose email contains `search` only (INVITATIONS-012).
pub(crate) async fn invitations(
    uow: &mut UnitOfWork,
    authorized: &Authorized<InvitationsRead>,
    search: Option<&Search>,
    page: &Page,
) -> Result<List<Invitation>, InvitationsError> {
    let rows = repo::page(uow, authorized.workspace_id(), search, page).await?;
    Ok(page.list(rows, |row| key(row.created_at, row.id)))
}

/// The invitation `id` of the caller's workspace, locked until the unit of work ends, if it offers
/// one of `offerable`, the roles that `access` lets the caller offer; otherwise the refusal,
/// through `authz::refuse`, which rolls the unit of work back (INVITATIONS-014, DATA-20).
async fn hold<P: Permission>(
    mut uow: UnitOfWork,
    authorized: &Authorized<P>,
    offerable: &[Role],
    id: uuid::Uuid,
) -> Result<(UnitOfWork, CurrentInvitation), InvitationsError> {
    let Some(held) = repo::hold(&mut uow, authorized.workspace_id(), id).await? else {
        return Err(InvitationsError::NotFound);
    };
    if !offerable.contains(&held.invitation.role) {
        return Err(authz::refuse(uow, authorized).await.into());
    }
    Ok((uow, held))
}

/// Records `action` on the invitation `held` under the proof's scope, with `details`, and with
/// the account that holds its address as the entry's target, which it returns: the user its
/// event goes to (Who an invitation reaches).
async fn record<P: Permission, D: Serialize>(
    uow: &mut UnitOfWork,
    authorized: &Authorized<P>,
    action: log::Action<D>,
    held: &CurrentInvitation,
    details: D,
) -> Result<Option<platform::types::UserId>, InvitationsError> {
    let invitee = accounts::find_by_email(uow, &held.invitation.email).await?;
    let entry =
        Entry::detailed(action, audit::INVITATION, held.invitation.id, details).targeting(invitee);
    log::record(uow, authorized.audit_scope(), entry).await?;
    Ok(invitee)
}

/// Changes the role of the invitation `id` of the caller's workspace to `new`, in the unit of work
/// it was authorized in: whatever the invitation's version when `versions` is `None`, and
/// otherwise only if its version is one of them. The role, its audit entry and the events commit
/// together; a role that the invitation offers already changes nothing (INVITATIONS-013,
/// INVITATIONS-014, PLATFORM-071).
pub(crate) async fn change_role(
    uow: UnitOfWork,
    authorized: &Authorized<InvitationsUpdate>,
    id: uuid::Uuid,
    new: NewRole,
    versions: Option<&[Version]>,
) -> Result<CurrentInvitation, InvitationsError> {
    let offerable = access::offerable(authorized)?;
    if !offerable.contains(&new.role) {
        return Err(authz::refuse(uow, authorized).await.into());
    }
    let (mut uow, held) = hold(uow, authorized, &offerable, id).await?;
    if versions.is_some_and(|versions| !versions.contains(&held.version)) {
        return Err(RequestError::PreconditionFailed.into());
    }
    if held.invitation.role == new.role {
        uow.commit().await?;
        return Ok(held);
    }
    let workspace = authorized.workspace_id();
    let Some(current) = repo::set_role(&mut uow, workspace, id, new.role, versions).await? else {
        return Err(RequestError::PreconditionFailed.into());
    };
    let details = ChangedRole {
        from: held.invitation.role,
        to: new.role,
    };
    let action = audit::INVITATION_ROLE_CHANGED;
    let invitee = record(&mut uow, authorized, action, &held, details).await?;
    announce(&mut uow, &published::INVITATION_UPDATED, workspace, id).await?;
    if let Some(user) = invitee {
        tell(
            &mut uow,
            &published::INVITATION_REVISED,
            user,
            workspace,
            id,
        )
        .await?;
    }
    uow.commit().await?;
    Ok(current)
}

/// Sends the invitation `id` of the caller's workspace again, once the caller's bucket has
/// counted the request (INVITATIONS-009), in the unit of work `authorize` opens: the caller
/// becomes its inviter, it expires the configured days from then, and its audit entry, its email
/// and the events commit together (INVITATIONS-014, INVITATIONS-015). `None` is an ID that isn't
/// a UUID, which names no invitation.
pub(crate) async fn resend(
    limits: &RateLimits,
    authorize: Authorize<InvitationsCreate>,
    id: Option<uuid::Uuid>,
    invitations: &Invitations,
) -> Result<CurrentInvitation, InvitationsError> {
    let inviter = authorize.principal().user_id();
    // Counted before the unit of work opens, as inviting is, so a request refused afterwards
    // still counts (PLATFORM-011).
    limits
        .check(invitations.settings.limit, inviter.uuid().as_bytes())
        .await?;
    let (uow, authorized) = authorize.open().await?;
    let authorized = &authorized;
    let id = id.ok_or(InvitationsError::NotFound)?;
    let offerable = access::offerable(authorized)?;
    let (mut uow, held) = hold(uow, authorized, &offerable, id).await?;
    let workspace = authorized.workspace_id();
    let days = invitations.settings.expiry_days;
    let current = repo::renew(&mut uow, workspace, id, inviter, days).await?;
    let action = audit::INVITATION_RESENT;
    let invitee = record(&mut uow, authorized, action, &held, Default::default()).await?;
    announce(&mut uow, &published::INVITATION_UPDATED, workspace, id).await?;
    let invitation = held.invitation;
    send_email(
        &mut uow,
        invitations,
        authorized,
        invitation.email,
        invitation.role,
    )
    .await?;
    if let Some(user) = invitee {
        tell(
            &mut uow,
            &published::INVITATION_RECEIVED,
            user,
            workspace,
            id,
        )
        .await?;
    }
    uow.commit().await?;
    Ok(current)
}

/// Cancels the invitation `id` of the caller's workspace, expired or not, in the unit of work it
/// was authorized in: its deletion, its audit entry and the events commit together
/// (INVITATIONS-014, INVITATIONS-016).
pub(crate) async fn cancel(
    uow: UnitOfWork,
    authorized: &Authorized<InvitationsCancel>,
    id: uuid::Uuid,
) -> Result<(), InvitationsError> {
    let offerable = access::offerable(authorized)?;
    let (mut uow, held) = hold(uow, authorized, &offerable, id).await?;
    let workspace = authorized.workspace_id();
    repo::cancel(&mut uow, workspace, id).await?;
    let action = audit::INVITATION_CANCELLED;
    let invitee = record(&mut uow, authorized, action, &held, Default::default()).await?;
    announce(&mut uow, &published::INVITATION_CANCELLED, workspace, id).await?;
    if let Some(user) = invitee {
        tell(
            &mut uow,
            &published::INVITATION_WITHDRAWN,
            user,
            workspace,
            id,
        )
        .await?;
    }
    Ok(uow.commit().await?)
}
