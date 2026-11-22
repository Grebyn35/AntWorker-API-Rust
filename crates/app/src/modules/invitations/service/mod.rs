//! Inviting, an invitee's own invitations, accepting and declining, with what a workspace's
//! members do with its invitations in `managing` (the invitations spec).

mod managing;

use std::collections::HashMap;

use super::{
    audit::{self, OfferedRole},
    emails,
    error::InvitationsError,
    events::{self as published, AddressedInvitation, InvitationChanged},
    model::{Accepted, Invited, NewInvitation, ReceivedInvitation, Settings},
    repo,
};
use crate::modules::{
    access::{self, InvitationsCreate, InvitationsRead, Role},
    accounts::{self, DisplayName, Profile, PublicProfile},
    audit::{self as log, Entry},
    workspaces::{self, WorkspaceName},
};
use platform::{
    authn::Principal,
    authz::{self, Authorize, Authorized},
    db::{Database, UnitOfWork},
    errors::Internal,
    events::{UserEvent, WorkspaceEvent},
    idempotency::{Claimed, IdempotencyKey},
    lists::{List, Page, Search, key},
    mail::Mail,
    rate_limit::RateLimits,
    types::{Client, EmailAddress, UserId, WorkspaceId},
};

pub(crate) use managing::{cancel, change_role, invitation, invitations, resend};

/// What the routes of `invitations` share: the database, the emails, how long an invitation
/// lasts, and how many a member may send.
#[derive(Clone)]
pub(crate) struct Invitations {
    pub(super) database: Database,
    pub(super) mail: Mail,
    pub(super) settings: Settings,
}

impl Invitations {
    /// The module's state, on `database`, sending its emails through `mail`.
    pub(crate) fn new(database: Database, mail: Mail, settings: Settings) -> Self {
        Self {
            database,
            mail,
            settings,
        }
    }
}

/// Invites `input.email` to the caller's workspace, once the caller's bucket has counted the
/// request (INVITATIONS-009), in the unit of work `authorize` opens, which claims `key` first, so
/// a retry with the same key and request answers the same invitation (INVITATIONS-001 to
/// INVITATIONS-003, INVITATIONS-008, PLATFORM-036).
pub(crate) async fn invite(
    limits: &RateLimits,
    authorize: Authorize<InvitationsCreate>,
    key: Option<&IdempotencyKey>,
    input: NewInvitation,
    invitations: &Invitations,
) -> Result<Invited, InvitationsError> {
    let inviter = authorize.principal().user_id();
    // Counted before the unit of work opens, so a request refused afterwards still counts
    // (PLATFORM-011).
    limits
        .check(invitations.settings.limit, inviter.uuid().as_bytes())
        .await?;
    let (mut uow, authorized) = authorize.open().await?;
    let authorized = &authorized;
    if let Claimed::Replay(invited) = uow.claim(inviter, key).await? {
        return Ok(invited);
    }
    let offerable = access::offerable(authorized)?;
    if !offerable.contains(&input.role) {
        return Err(authz::refuse(uow, authorized).await.into());
    }
    let workspace = authorized.workspace_id();
    let invitee = accounts::find_by_email(&mut uow, &input.email).await?;
    if let Some(user) = invitee
        && access::is_member(&mut uow, workspace, user).await?
    {
        return Err(InvitationsError::AlreadyMember);
    }
    let Some((current, created)) = repo::upsert(
        &mut uow,
        workspace,
        (&input.email, input.role),
        inviter,
        invitations.settings.expiry_days,
        &offerable,
    )
    .await?
    else {
        // The address's invitation offers a role the caller may not offer (INVITATIONS-014).
        return Err(authz::refuse(uow, authorized).await.into());
    };
    let id = current.invitation.id;
    let (action, event) = if created {
        (audit::INVITATION_CREATED, &published::INVITATION_CREATED)
    } else {
        (audit::INVITATION_UPDATED, &published::INVITATION_UPDATED)
    };
    // Read again, and held, so that the entry's target and the event's user meet their
    // foreign keys: an account that a deletion took meanwhile is none. After the invitation's
    // row, the order in which a change, a sending again and a cancellation take the two, and
    // an acceptance and a decline the invitation and the invitee's row (DATA-20).
    let invitee = accounts::find_by_email_for_key_share(&mut uow, &input.email).await?;
    let entry = Entry::detailed(
        action,
        audit::INVITATION,
        id,
        OfferedRole { role: input.role },
    )
    .targeting(invitee);
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    announce(&mut uow, event, workspace, id).await?;
    send_email(&mut uow, invitations, authorized, input.email, input.role).await?;
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
    let invited = Invited { created, current };
    uow.commit_with(&invited).await?;
    Ok(invited)
}

/// Enqueues the invitation email to `to`, offering `role` in the caller's workspace, with the
/// caller as its inviter, under the proof's scope (INVITATIONS-010).
async fn send_email(
    uow: &mut UnitOfWork,
    invitations: &Invitations,
    authorized: &Authorized<InvitationsCreate>,
    to: EmailAddress,
    role: Role,
) -> Result<(), InvitationsError> {
    let email = emails::Invitation {
        to,
        inviter: inviter_name(uow, authorized.principal().user_id()).await?,
        workspace: workspace_name(uow, authorized).await?,
        role,
        expires_in_days: invitations.settings.expiry_days,
    }
    .render()?;
    Ok(invitations
        .mail
        .send(uow, authorized.job_scope(), email)
        .await?)
}

/// Publishes `event` about the invitation `id` to the members of `workspace` who may read its
/// invitations.
async fn announce(
    uow: &mut UnitOfWork,
    event: &WorkspaceEvent<InvitationsRead, InvitationChanged>,
    workspace: WorkspaceId,
    id: uuid::Uuid,
) -> Result<(), Internal> {
    let payload = InvitationChanged {
        workspace_id: workspace,
        invitation_id: id,
    };
    uow.publish_to_workspace(event, workspace, &payload).await
}

/// Publishes `event` about the invitation `id` of `workspace` to `user`, whose account holds its
/// address (Who an invitation reaches).
async fn tell(
    uow: &mut UnitOfWork,
    event: &UserEvent<AddressedInvitation>,
    user: UserId,
    workspace: WorkspaceId,
    id: uuid::Uuid,
) -> Result<(), Internal> {
    let payload = AddressedInvitation {
        invitation_id: id,
        workspace_id: workspace,
    };
    uow.publish_to_user(event, user, &payload).await
}

/// The inviter's name, for the email.
async fn inviter_name(
    uow: &mut UnitOfWork,
    inviter: UserId,
) -> Result<DisplayName, InvitationsError> {
    let profile = accounts::profiles(uow, &[inviter])
        .await?
        .into_iter()
        .next();
    let profile = profile.ok_or_else(|| Internal::new("the inviter's user doesn't exist"))?;
    Ok(profile.name)
}

/// The name of the workspace the caller invites to, or sends an invitation of again, for the
/// email.
async fn workspace_name(
    uow: &mut UnitOfWork,
    authorized: &Authorized<InvitationsCreate>,
) -> Result<WorkspaceName, InvitationsError> {
    let names = workspaces::names(uow, &[authorized.workspace_id()]).await?;
    let (_, name) = names
        .into_iter()
        .next()
        .ok_or_else(|| Internal::new("the caller's workspace doesn't exist"))?;
    Ok(name)
}

/// A page of the caller's unexpired invitations in every workspace, the latest first, to the
/// workspaces whose name contains `search` only (INVITATIONS-004).
pub(crate) async fn received(
    database: &Database,
    client: &Client,
    principal: &Principal,
    search: Option<&Search>,
    page: &Page,
) -> Result<List<ReceivedInvitation>, InvitationsError> {
    let mut uow = database.begin(client, Some(principal)).await?;
    let Profile { email, .. } = accounts::profile(&mut uow, principal).await?;
    let only = match search {
        None => None,
        Some(search) => {
            let all = repo::workspace_ids(&mut uow, &email).await?;
            Some(workspaces::matching(&mut uow, &all, search).await?)
        }
    };
    let rows = repo::pending(&mut uow, &email, only.as_deref(), page).await?;
    let pending = page.list(rows, |row| key(row.created_at, row.id));
    let workspace_ids: Vec<_> = pending.items.iter().map(|row| row.workspace_id).collect();
    let inviter_ids: Vec<_> = pending
        .items
        .iter()
        .filter_map(|row| row.inviter_id)
        .collect();
    let names: HashMap<_, _> = workspaces::names(&mut uow, &workspace_ids)
        .await?
        .into_iter()
        .collect();
    let inviters: HashMap<UserId, PublicProfile> = accounts::profiles(&mut uow, &inviter_ids)
        .await?
        .into_iter()
        .map(|profile| (profile.id, profile))
        .collect();
    // An invitation whose workspace went between the reads drops out of the page. One whose
    // inviter's account is deleted, then or before, has no inviter's name (ACCOUNTS-014).
    let items = pending
        .items
        .into_iter()
        .filter_map(|row| {
            let workspace_name = names.get(&row.workspace_id)?.clone();
            let inviter_name = row
                .inviter_id
                .and_then(|inviter| inviters.get(&inviter))
                .map(|profile| profile.name.clone());
            Some(ReceivedInvitation {
                id: row.id,
                workspace_id: row.workspace_id,
                workspace_name,
                inviter_name,
                role: row.role,
                expires_at: row.expires_at,
            })
        })
        .collect();
    Ok(List {
        items,
        next_cursor: pending.next_cursor,
    })
}

/// Accepts the invitation `id` addressed to the caller: their membership, the invitation's
/// deletion, its audit entry and the events commit together (INVITATIONS-005,
/// INVITATIONS-006). `None` is an ID that isn't a UUID, which names no invitation.
pub(crate) async fn accept(
    database: &Database,
    client: &Client,
    principal: &Principal,
    id: Option<uuid::Uuid>,
) -> Result<Accepted, InvitationsError> {
    let id = id.ok_or(InvitationsError::NotFound)?;
    let mut uow = database.begin(client, Some(principal)).await?;
    let Profile { email, .. } = accounts::profile(&mut uow, principal).await?;
    let workspace = repo::workspace_of(&mut uow, id, &email)
        .await?
        .ok_or(InvitationsError::NotFound)?;
    // The owner's membership first, then the invitation, the order of the owner's deletion, which
    // deletes the invitation with the workspace (DATA-20, ACCESS-008).
    if !access::hold_owner(&mut uow, workspace).await? {
        return Err(InvitationsError::NotFound);
    }
    let found = repo::find(&mut uow, id, &email)
        .await?
        .ok_or(InvitationsError::NotFound)?;
    if found.expired {
        return Err(InvitationsError::Expired);
    }
    let user = principal.user_id();
    if access::add_member(&mut uow, workspace, user, found.role)
        .await?
        .is_none()
    {
        return Err(InvitationsError::AlreadyMember);
    }
    repo::delete(&mut uow, id, &email).await?;
    #[expect(
        clippy::disallowed_methods,
        reason = "accepting an invitation is how its invitee joins (the invitations spec)"
    )]
    let scope = principal.invitee_scope(workspace);
    let entry = Entry::new(audit::INVITATION_ACCEPTED, audit::INVITATION, id);
    log::record(&mut uow, scope, entry).await?;
    let payload = published::MemberJoined {
        workspace_id: workspace,
        user_id: user,
    };
    uow.publish_to_workspace(&published::MEMBER_JOINED, workspace, &payload)
        .await?;
    let payload = workspaces::MembershipCreated {
        workspace_id: workspace,
        session_id: principal.session_id(),
    };
    uow.publish_to_user(&workspaces::MEMBERSHIP_CREATED, user, &payload)
        .await?;
    uow.commit().await?;
    Ok(Accepted {
        workspace_id: workspace,
        role: found.role,
    })
}

/// Declines the invitation `id` addressed to the caller, expired or not: its deletion, its audit
/// entry and the members' event commit together (INVITATIONS-017). `None` is an ID that isn't a
/// UUID, which names no invitation.
pub(crate) async fn decline(
    database: &Database,
    client: &Client,
    principal: &Principal,
    id: Option<uuid::Uuid>,
) -> Result<(), InvitationsError> {
    let id = id.ok_or(InvitationsError::NotFound)?;
    let mut uow = database.begin(client, Some(principal)).await?;
    let Profile { email, .. } = accounts::profile(&mut uow, principal).await?;
    let workspace = repo::workspace_of(&mut uow, id, &email)
        .await?
        .ok_or(InvitationsError::NotFound)?;
    // The owner's membership first, then the invitation, as accepting takes them (DATA-20).
    if !access::hold_owner(&mut uow, workspace).await? {
        return Err(InvitationsError::NotFound);
    }
    if !repo::delete(&mut uow, id, &email).await? {
        return Err(InvitationsError::NotFound);
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "declining an invitation is its invitee's answer to the workspace (the invitations spec)"
    )]
    let scope = principal.invitee_scope(workspace);
    let entry = Entry::new(audit::INVITATION_DECLINED, audit::INVITATION, id);
    log::record(&mut uow, scope, entry).await?;
    announce(&mut uow, &published::INVITATION_DECLINED, workspace, id).await?;
    Ok(uow.commit().await?)
}
