//! Starting a session, with new tokens, in place of the sessions that its request shows: the one
//! its user has on the device, and the one whose cookies the request's browser sent (AUTH-008,
//! AUTH-096, the auth spec's "Sign-in").

use platform::{
    authn::{AuditScope, Principal},
    db::UnitOfWork,
    errors::Internal,
    hashing::sha256,
    types::{Secret, SessionId, UserId},
};

use super::{Auth, Device, new_tokens, sessions};
use crate::modules::{
    audit::{self as audit_log, Entry},
    auth::{
        audit::{self, ThroughProvider},
        error::AuthError,
        events::{SESSION_CREATED, SessionChanged},
        model::{StoredSession, Tokens},
        repo,
    },
};

/// The tokens of the session that a request's browser holds, as its session cookies carry them,
/// which a session that the request starts replaces (AUTH-096).
#[derive(Default)]
pub(crate) struct Held {
    /// What the access cookie holds, if the request has one.
    pub(crate) access: Option<Secret<String>>,
    /// What the refresh cookie holds, if the request has one.
    pub(crate) refresh: Option<Secret<String>>,
}

impl Auth {
    /// Starts a session of `user_id` on `device`, ending the one its device has and the one its
    /// request's browser holds, if any, through the full sign-out path, and records
    /// `session.created` (AUTH-008, AUTH-096).
    pub(super) async fn replace_session(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        device: &Device,
    ) -> Result<(Principal, StoredSession, Tokens), AuthError> {
        let on_device = match &device.id {
            Some(device_id) => repo::session_on_device(uow, user_id, device_id)
                .await?
                .map(|id| (user_id, id)),
            None => None,
        };
        let ended = delete_shown(uow, on_device, &device.held).await?;
        let (principal, session, tokens) = self.insert_session(uow, user_id, device).await?;
        record_ended(uow, &principal, &ended).await?;
        let entry = Entry::detailed(
            audit::SESSION_CREATED,
            audit::SESSION,
            session.id.uuid(),
            ThroughProvider::new(device.provider.as_deref()),
        );
        audit_log::record(uow, principal.audit_scope(), entry).await?;
        Ok((principal, session, tokens))
    }

    /// Starts the first session of `user_id`, whom the unit of work registers, on `device`,
    /// ending the one its request's browser holds, if any, through the full sign-out path
    /// (AUTH-001, AUTH-074, AUTH-096).
    pub(super) async fn start_session(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        device: &Device,
    ) -> Result<(Principal, StoredSession, Tokens), AuthError> {
        let ended = delete_shown(uow, None, &device.held).await?;
        let (principal, session, tokens) = self.insert_session(uow, user_id, device).await?;
        record_ended(uow, &principal, &ended).await?;
        Ok((principal, session, tokens))
    }

    /// Creates a session of `user_id` on `device`, with new tokens, and the principal it proves,
    /// and publishes `session.created` to the user's streams (AUTH-046).
    async fn insert_session(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        device: &Device,
    ) -> Result<(Principal, StoredSession, Tokens), AuthError> {
        let tokens = new_tokens()?;
        let session = repo::insert_session(
            uow,
            user_id,
            (
                device.id.as_ref(),
                device.name.as_ref(),
                device.provider.as_deref(),
            ),
            &sha256(tokens.access.expose().as_bytes()),
            &sha256(tokens.refresh.expose().as_bytes()),
            self.settings.lifetimes,
        )
        .await?;
        #[expect(
            clippy::disallowed_methods,
            reason = "auth builds the principal of a session it just created (the auth spec)"
        )]
        let principal = Principal::new(user_id, session.id);
        let payload = SessionChanged {
            session_id: session.id,
        };
        uow.publish_to_user(&SESSION_CREATED, user_id, &payload)
            .await?;
        Ok((principal, session, tokens))
    }
}

/// Deletes the sessions that a request that starts a session shows, and returns those that were
/// there to delete, each with its user: `on_device`, the one its user has on the device, and the
/// one that `held`'s tokens name, by its access token or else by its refresh token, whoever its
/// user is. It deletes them in the order of their IDs, so two such requests that each show the
/// session the other's device has wait for one another's first, and never deadlock (AUTH-008,
/// AUTH-096, DATA-20).
async fn delete_shown(
    uow: &mut UnitOfWork,
    on_device: Option<(UserId, SessionId)>,
    held: &Held,
) -> Result<Vec<(UserId, SessionId)>, Internal> {
    let mut shown: Vec<(UserId, SessionId)> = on_device.into_iter().collect();
    if let Some(held) = held_session(uow, held).await?
        && !shown.contains(&held)
    {
        shown.push(held);
    }
    shown.sort_by_key(|(_, session_id)| session_id.uuid());
    let mut ended = Vec::new();
    for (user_id, session_id) in shown {
        if repo::delete_session(uow, user_id, session_id).await? {
            ended.push((user_id, session_id));
        }
    }
    Ok(ended)
}

/// The session that `held`'s tokens name, with its user: the one of its access token, or, when
/// that names none, the one of its refresh token (AUTH-096).
async fn held_session(
    uow: &mut UnitOfWork,
    held: &Held,
) -> Result<Option<(UserId, SessionId)>, Internal> {
    if let Some(access) = &held.access
        && let Some(found) = repo::by_access_token(uow, &sha256(access.expose().as_bytes())).await?
    {
        return Ok(Some((found.user_id, found.id)));
    }
    if let Some(refresh) = &held.refresh
        && let Some(found) =
            repo::by_refresh_token(uow, &sha256(refresh.expose().as_bytes())).await?
    {
        return Ok(Some((found.user_id, found.id)));
    }
    Ok(None)
}

/// Signals, publishes and records the end of each of `ended`, the sessions that the unit of work
/// deleted for the session of `principal`, which it started: one of the principal's own user in
/// the principal's scope, and one of another user in that user's log, with no actor (AUTH-008,
/// AUTH-096).
async fn record_ended(
    uow: &mut UnitOfWork,
    principal: &Principal,
    ended: &[(UserId, SessionId)],
) -> Result<(), AuthError> {
    for &(user_id, session_id) in ended {
        let scope = if user_id == principal.user_id() {
            principal.audit_scope()
        } else {
            AuditScope::attempt(user_id)
        };
        sessions::end_sessions(uow, scope, user_id, &[session_id]).await?;
    }
    Ok(())
}
