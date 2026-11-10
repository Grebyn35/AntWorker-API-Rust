//! Re-authenticating a session with its user's password, the one check of a signed-in user's
//! password, which changing the password goes through too, and the check that a session proved it
//! just before a sensitive action (AUTH-021 to AUTH-023, AUTH-040, AUTH-044, AUTH-045, the auth
//! spec's "Re-authentication").

use platform::{
    authn::Principal,
    db::UnitOfWork,
    errors::RequestError,
    rate_limit::RateLimits,
    types::{Client, UserId},
};

use super::{Auth, buckets, verify};
use crate::modules::{
    accounts,
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        error::AuthError,
        model::{Password, Session},
        repo,
    },
};

/// Refuses, unless the session that authenticated the caller proved their password within the
/// last 5 minutes, by the database's clock, as its `authenticated_at` records, read in `uow`
/// without a lock (AUTH-023, AUTH-044, AUTH-045). A session that another request ended while
/// this one waited answers 401, as a request that came after the end does (AUTH-009).
pub(super) async fn require_recent_proof(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<(), AuthError> {
    match repo::recently_authenticated(uow, principal.user_id(), principal.session_id()).await? {
        Some(true) => Ok(()),
        Some(false) => Err(AuthError::ReauthenticationRequired),
        None => Err(RequestError::Unauthenticated.into()),
    }
}

/// Takes the caller's turn at ending their sessions: locks their user's row against every other
/// change until `uow` ends, as a reset and a deletion of the account lock it first, so that two
/// requests that each end the other's session take turns, and the second finds its own session
/// gone (AUTH-040, AUTH-044, AUTH-045, DATA-20), and gives the user's address. A user that a
/// deletion erased while this request waited answers 401, as the request's session went with them.
pub(super) async fn take_turn(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<accounts::Address, AuthError> {
    accounts::address_for_update(uow, principal.user_id())
        .await?
        .ok_or_else(|| RequestError::Unauthenticated.into())
}

impl Auth {
    /// Re-authenticates the session that authenticated the caller with their `password`, and
    /// answers it, or records a wrong password in their log (AUTH-021).
    pub(crate) async fn reauthenticate(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        password: Password,
    ) -> Result<Session, AuthError> {
        let user_id = principal.user_id();
        let (mut uow, matches) = self
            .check_password(limits, client, user_id, password)
            .await?;
        if !matches {
            return Err(self.wrong_password(uow, principal).await?);
        }
        // Another request may have ended the session since it authenticated this one.
        let session = repo::reauthenticate(&mut uow, user_id, principal.session_id())
            .await?
            .ok_or(RequestError::Unauthenticated)?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(
                audit::SESSION_REAUTHENTICATED,
                audit::SESSION,
                session.id.uuid(),
            ),
        )
        .await?;
        let session = self.answer(&mut uow, session).await?;
        uow.commit().await?;
        Ok(session)
    }

    /// Records in `uow` that the caller's password was wrong, a security failure, which commits
    /// before the answer, and returns the answer (AUTH-021, AUTH-040, ARCH-11).
    pub(super) async fn wrong_password(
        &self,
        mut uow: UnitOfWork,
        principal: &Principal,
    ) -> Result<AuthError, AuthError> {
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(
                audit::PASSWORD_CHECK_FAILED,
                audit::USER,
                principal.user_id().uuid(),
            ),
        )
        .await?;
        uow.commit().await?;
        Ok(AuthError::WrongPassword)
    }

    /// Whether `password` is `user_id`'s, with the operation's unit of work, open: the one check of
    /// a signed-in user's password, which counts in their bucket before the unit of work opens,
    /// then reads their password identity in it and verifies the password through `workers`,
    /// against the dummy hash when they have none, so a missing password costs what a wrong one
    /// does (AUTH-021, AUTH-022, SEC-13, ARCH-11).
    pub(super) async fn check_password(
        &self,
        limits: &RateLimits,
        client: &Client,
        user_id: UserId,
        password: Password,
    ) -> Result<(UnitOfWork, bool), AuthError> {
        limits
            .check(buckets::PASSWORD_CHECK, user_id.uuid().as_bytes())
            .await?;
        let mut uow = self.database.begin(client).await?;
        let stored = repo::password_hash(&mut uow, user_id).await?;
        let has_password = stored.is_some();
        let hash = stored.unwrap_or_else(|| self.dummy_hash.clone());
        let (matches, _) = verify(&self.workers, hash, password).await?;
        Ok((uow, matches && has_password))
    }
}
