//! A user's sessions: reading the current one, listing them, and ending them, the current one,
//! which is signing out, another, or every other (AUTH-013, AUTH-016, AUTH-043 to AUTH-046, the
//! auth spec's "A user's sessions"). Every way a session ends but a deletion of the account goes
//! through [`close`].

use platform::{
    authn::{AuditScope, Principal},
    db::UnitOfWork,
    errors::Internal,
    events::{self, AccessChange},
    lists::{List, Page, Search, key},
    types::{Client, SessionId, UserId},
};

use super::{
    Auth,
    reauthentication::{require_recent_proof, take_turn},
};
use crate::modules::{
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        error::AuthError,
        events::{SESSION_ENDED, SessionChanged},
        model::{Session, SessionItem},
        repo,
    },
};

/// Which session a request ended (AUTH-044).
pub(crate) enum Ended {
    /// The session that authenticated the request: it signed out.
    Own,
    /// Another of the user's sessions.
    Other,
}

impl Auth {
    /// The session that authenticated the caller (AUTH-016).
    pub(crate) async fn current(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<Session, AuthError> {
        let mut uow = self.database.begin(client).await?;
        let session = repo::session(&mut uow, principal.user_id(), principal.session_id())
            .await?
            .ok_or_else(|| Internal::new("the caller's session doesn't exist"))?;
        self.answer(&mut uow, session).await
    }

    /// A page of the caller's sessions that last, the latest started first, kept to those whose
    /// device's name contains `search` (AUTH-043).
    pub(crate) async fn sessions(
        &self,
        client: &Client,
        principal: &Principal,
        search: Option<&Search>,
        page: &Page,
    ) -> Result<List<SessionItem>, AuthError> {
        let mut uow = self.database.begin(client).await?;
        let current = principal.session_id();
        let items = repo::page(&mut uow, principal.user_id(), search, page)
            .await?
            .into_iter()
            .map(|session| session.item(current))
            .collect();
        Ok(page.list(items, |item| key(item.created_at, item.id.uuid())))
    }

    /// Ends the session that authenticated the caller, unless another request ended it first
    /// (AUTH-013).
    pub(crate) async fn sign_out(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<(), AuthError> {
        let mut uow = self.database.begin(client).await?;
        let (user_id, session_id) = (principal.user_id(), principal.session_id());
        if repo::delete_session(&mut uow, user_id, session_id).await? {
            end_sessions(&mut uow, principal.audit_scope(), user_id, &[session_id]).await?;
        }
        uow.commit().await?;
        Ok(())
    }

    /// Ends the caller's session `id`, which `None` gives for a path ID that isn't a UUID: their
    /// own, which signs them out, or, once their session proved their password within the last 5
    /// minutes, another, at their turn at ending their sessions (AUTH-044, PLATFORM-072).
    pub(crate) async fn end_session(
        &self,
        client: &Client,
        principal: &Principal,
        id: Option<SessionId>,
    ) -> Result<Ended, AuthError> {
        if id == Some(principal.session_id()) {
            self.sign_out(client, principal).await?;
            return Ok(Ended::Own);
        }
        let mut uow = self.database.begin(client).await?;
        take_turn(&mut uow, principal).await?;
        require_recent_proof(&mut uow, principal).await?;
        let user_id = principal.user_id();
        let Some(id) = id else {
            return Err(AuthError::SessionNotFound);
        };
        if !repo::delete_session(&mut uow, user_id, id).await? {
            return Err(AuthError::SessionNotFound);
        }
        end_sessions(&mut uow, principal.audit_scope(), user_id, &[id]).await?;
        uow.commit().await?;
        Ok(Ended::Other)
    }

    /// Ends every session of the caller's but the one that authenticated the request, once it
    /// proved their password within the last 5 minutes, at their turn at ending their sessions
    /// (AUTH-045).
    pub(crate) async fn end_other_sessions(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<(), AuthError> {
        let mut uow = self.database.begin(client).await?;
        take_turn(&mut uow, principal).await?;
        require_recent_proof(&mut uow, principal).await?;
        let user_id = principal.user_id();
        let ended = repo::delete_other_sessions(&mut uow, user_id, principal.session_id()).await?;
        end_sessions(&mut uow, principal.audit_scope(), user_id, &ended).await?;
        uow.commit().await?;
        Ok(())
    }
}

/// [`close`]s each of `user_id`'s sessions in `ended`, which the unit of work deleted, and
/// records each as `session.ended` in `scope` (AUTH-008, AUTH-013, AUTH-037, AUTH-040, AUTH-044,
/// AUTH-045).
pub(super) async fn end_sessions(
    uow: &mut UnitOfWork,
    scope: AuditScope,
    user_id: UserId,
    ended: &[SessionId],
) -> Result<(), AuthError> {
    close(uow, user_id, ended).await?;
    for &session_id in ended {
        audit_log::record(
            uow,
            scope,
            Entry::new(audit::SESSION_ENDED, audit::SESSION, session_id.uuid()),
        )
        .await?;
    }
    Ok(())
}

/// Signals that each of `user_id`'s sessions in `ended`, which the unit of work deleted, ended,
/// so its streams close when the unit of work commits, and publishes `session.ended` for each to
/// the user's streams (AUTH-046, D35).
pub(super) async fn close(
    uow: &mut UnitOfWork,
    user_id: UserId,
    ended: &[SessionId],
) -> Result<(), Internal> {
    for &session_id in ended {
        events::signal(
            uow,
            AccessChange::SessionEnded {
                user_id,
                session_id,
            },
        )
        .await?;
        uow.publish_to_user(&SESSION_ENDED, user_id, &SessionChanged { session_id })
            .await?;
    }
    Ok(())
}
