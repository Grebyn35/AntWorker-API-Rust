//! Inviting, an invitee's own invitations, and accepting (the invitations spec).

use std::collections::HashMap;

use platform::{
    authn::Principal,
    authz::{self, Authorize, Authorized},
    db::{Database, UnitOfWork},
    errors::Internal,
    idempotency::{Claimed, IdempotencyKey},
    lists::{List, Page, Search, key},
    rate_limit::RateLimits,
    types::{Client, UserId},
};
use serde_json::json;

use super::{
    audit,
    error::InvitationsError,
    events as published,
    model::{Accepted, Invited, NewInvitation, ReceivedInvitation, Settings},
    repo,
};
use crate::modules::{
    access::{self, InvitationsCreate, Role},
    accounts::{self, PublicProfile},
    audit::{self as log, Entry},
    mail::{self, Mail, Message},
    workspaces,
};

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
/// INVITATIONS-003, INVITATIONS-008, PLATFORM-018).
pub(crate) async fn invite(
    limits: &RateLimits,
    authorize: Authorize<InvitationsCreate>,
    key: Option<&IdempotencyKey>,
    input: NewInvitation,
    invitations: &Invitations,
) -> Result<Invited, InvitationsError> {
    let inviter = authorize.principal().user_id();
    // Counted before the unit of work opens, so a request refused afterwards still counts
    // (PLATFORM-026).
    limits
        .check(invitations.settings.limit, inviter.uuid().as_bytes())
        .await?;
    let (mut uow, authorized) = authorize.open().await?;
    let authorized = &authorized;
    if let Claimed::Replay(invited) = uow.claim(inviter, key).await? {
        return Ok(invited);
    }
    if !Role::from_name(authorized.role())?.outranks(input.role) {
        return Err(authz::refuse(uow, authorized).await.into());
    }
    let workspace = authorized.workspace_id();
    let invitee = accounts::find_by_email(&mut uow, &input.email).await?;
    if let Some(user) = invitee
        && access::is_member(&mut uow, workspace, user).await?
    {
        return Err(InvitationsError::AlreadyMember);
    }
    let expiry_days = invitations.settings.expiry_days;
    let (invitation, created) = repo::upsert(
        &mut uow,
        workspace,
        &input.email,
        input.role,
        inviter,
        expiry_days,
    )
    .await?;
    let action = if created {
        audit::INVITATION_CREATED
    } else {
        audit::INVITATION_UPDATED
    };
    let mut entry = Entry::new(action, audit::INVITATION, invitation.id);
    entry.target = invitee;
    entry.details = json!({ "role": input.role });
    log::record(&mut uow, authorized.audit_scope(), entry).await?;
    let message = Message::Invitation(mail::Invitation {
        to: input.email,
        inviter: inviter_name(&mut uow, inviter).await?,
        workspace: workspace_name(&mut uow, authorized).await?,
        role: input.role.name().to_owned(),
        expires_in_days: expiry_days,
    });
    invitations
        .mail
        .send(&mut uow, authorized.job_scope(), message)
        .await?;
    if let Some(user) = invitee {
        let payload = json!({ "invitation_id": invitation.id, "workspace_id": workspace });
        uow.publish_to_user(&published::INVITATION_RECEIVED, user, &payload)
            .await?;
    }
    let invited = Invited {
        created,
        invitation,
    };
    uow.commit_with(&invited).await?;
    Ok(invited)
}

/// The inviter's name, for the email.
async fn inviter_name(uow: &mut UnitOfWork, inviter: UserId) -> Result<String, InvitationsError> {
    let profile = accounts::profiles(uow, &[inviter])
        .await?
        .into_iter()
        .next();
    let profile = profile.ok_or_else(|| Internal::new("the inviter's user doesn't exist"))?;
    Ok(profile.name.as_str().to_owned())
}

/// The name of the workspace the caller invites to, for the email.
async fn workspace_name(
    uow: &mut UnitOfWork,
    authorized: &Authorized<InvitationsCreate>,
) -> Result<String, InvitationsError> {
    let names = workspaces::names(uow, &[authorized.workspace_id()]).await?;
    let (_, name) = names
        .into_iter()
        .next()
        .ok_or_else(|| Internal::new("the caller's workspace doesn't exist"))?;
    Ok(name.as_str().to_owned())
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
    let mut uow = database.begin(client).await?;
    let email = accounts::profile(&mut uow, principal).await?.email;
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
    let inviter_ids: Vec<_> = pending.items.iter().map(|row| row.inviter_id).collect();
    let names: HashMap<_, _> = workspaces::names(&mut uow, &workspace_ids)
        .await?
        .into_iter()
        .collect();
    let inviters: HashMap<UserId, PublicProfile> = accounts::profiles(&mut uow, &inviter_ids)
        .await?
        .into_iter()
        .map(|profile| (profile.id, profile))
        .collect();
    // An invitation whose workspace or inviter went between the reads drops out of the page.
    let items = pending
        .items
        .into_iter()
        .filter_map(|row| {
            let workspace_name = names.get(&row.workspace_id)?.clone();
            let inviter_name = inviters.get(&row.inviter_id)?.name.clone();
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
    let mut uow = database.begin(client).await?;
    let email = accounts::profile(&mut uow, principal).await?.email;
    let found = repo::find(&mut uow, id, &email)
        .await?
        .ok_or(InvitationsError::NotFound)?;
    if found.expired {
        return Err(InvitationsError::Expired);
    }
    let (user, workspace) = (principal.user_id(), found.workspace_id);
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
    let scope = principal.joiner_scope(workspace);
    let entry = Entry::new(audit::INVITATION_ACCEPTED, audit::INVITATION, id);
    log::record(&mut uow, scope, entry).await?;
    let payload = json!({ "workspace_id": workspace, "user_id": user });
    uow.publish_to_workspace(&published::MEMBER_JOINED, workspace, &payload)
        .await?;
    // The session that caused the event, so its own device can tell it already knows.
    let payload = json!({ "workspace_id": workspace, "session_id": principal.session_id() });
    uow.publish_to_user(&workspaces::MEMBERSHIP_CREATED, user, &payload)
        .await?;
    uow.commit().await?;
    Ok(Accepted {
        workspace_id: workspace,
        role: found.role,
    })
}
