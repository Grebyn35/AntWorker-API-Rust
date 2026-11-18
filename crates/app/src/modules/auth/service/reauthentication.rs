//! Re-authenticating a session with its user's password, the one check of a signed-in user's
//! password, which changing the password goes through too, and the check that a session proved it
//! just before a sensitive action (AUTH-021 to AUTH-023, AUTH-040, AUTH-044, AUTH-045, AUTH-048,
//! the auth spec's "Re-authentication").

use platform::{
    authn::Principal,
    blocking::Turn,
    db::UnitOfWork,
    errors::{Internal, RequestError},
    hashing::sha256,
    rate_limit::RateLimits,
    types::Client,
};

use super::{Auth, buckets, new_tokens, verify_in};
use crate::modules::{
    accounts,
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        error::AuthError,
        model::{FactorProof, Password, Session, StoredSession, Tokens},
        repo,
    },
};

/// Refuses, with 403 `request.reauthentication_required`, unless the session that authenticated
/// the caller proved who they are within the last 5 minutes, by the database's clock, as its
/// `authenticated_at` records, read in `uow` without a lock (AUTH-023, AUTH-044, AUTH-045,
/// AUTH-048), for a sensitive action of this module or of another, such as a transfer of a
/// workspace (WORKSPACES-018). A session that is gone since it authenticated the request, because
/// another request ended it or the hourly job deleted it, answers 401, as a request that came
/// after does (AUTH-009).
pub(crate) async fn require_recent_proof(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<(), AuthError> {
    match repo::recently_authenticated(uow, principal.user_id(), principal.session_id()).await? {
        Some(true) => Ok(()),
        Some(false) => Err(RequestError::ReauthenticationRequired.into()),
        None => Err(RequestError::Unauthenticated.into()),
    }
}

/// Takes the caller's turn at changing what proves who they are: locks their user's row against
/// every other change until `uow` ends, as a reset and a deletion of the account lock it first, so
/// that two requests that each end the other's session take turns, and the second finds its own
/// session gone (AUTH-044, AUTH-045), and two checks of their password take turns, and the second
/// reads the password the first set (AUTH-022, AUTH-040, DATA-20), and gives the user's address. A
/// user that a deletion erased while this request waited answers 401, as the request's session
/// went with them (ACCOUNTS-019).
pub(super) async fn take_turn(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<accounts::Address, AuthError> {
    accounts::address_for_update(uow, principal.user_id())
        .await?
        .ok_or_else(|| RequestError::Unauthenticated.into())
}

impl Auth {
    /// Re-authenticates the session that authenticated the caller with their `password`, and the
    /// second factor's code of an account that has one, and answers it, or records a wrong
    /// password or code in their log (AUTH-021, AUTH-065).
    pub(crate) async fn reauthenticate(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        password: Password,
        proof: Option<FactorProof>,
    ) -> Result<(Session, Tokens), AuthError> {
        self.count_signed_in_check(limits, principal, proof.as_ref())
            .await?;
        let (uow, _, matches, _) = self
            .check_password(limits, client, principal, password)
            .await?;
        if !matches {
            return Err(self.wrong_password(uow, principal).await?);
        }
        let mut uow = self
            .check_signed_in_factor(uow, principal, proof.as_ref())
            .await?;
        let (session, tokens) = self.proved(&mut uow, principal).await?;
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
        Ok((session, tokens))
    }

    /// Records in `uow` that the session that authenticated the caller proved who they are now,
    /// by the database's clock, and gives it a new pair of tokens, with which the pair it had
    /// stops working, as a refresh does: the one step of every proof in a session, a
    /// re-authentication's, with the password or through a provider, and a change of the
    /// password's (AUTH-095, AUTH-021, AUTH-040, AUTH-080). A session that another request ended
    /// since it authenticated this one answers 401.
    pub(super) async fn proved(
        &self,
        uow: &mut UnitOfWork,
        principal: &Principal,
    ) -> Result<(StoredSession, Tokens), AuthError> {
        let tokens = new_tokens()?;
        let session = repo::reauthenticate(
            uow,
            (principal.user_id(), principal.session_id()),
            &sha256(tokens.access.expose().as_bytes()),
            &sha256(tokens.refresh.expose().as_bytes()),
            self.settings.lifetimes,
        )
        .await?
        .ok_or(RequestError::Unauthenticated)?;
        Ok((session, tokens))
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

    /// Whether `password` is the caller's, with the operation's unit of work, open, their
    /// address, and the turn at hashing that the check took, in which its operation hashes what
    /// it must: the one check of a signed-in user's password, which counts in their bucket, then
    /// takes its turn at hashing, before the unit of work opens, so it waits for it holding no
    /// connection of the pool, then takes the user's turn in the unit of work, so it reads the
    /// password that the last change of it left, then reads their password identity and verifies
    /// the password, against the dummy hash when they have none, so a missing password costs
    /// what a wrong one does (AUTH-021, AUTH-022, AUTH-040, AUTH-093, SEC-13, ARCH-11, DATA-20).
    pub(super) async fn check_password(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        password: Password,
    ) -> Result<(UnitOfWork, accounts::Address, bool, Turn), AuthError> {
        let user_id = principal.user_id();
        limits
            .check(buckets::PASSWORD_CHECK, user_id.uuid().as_bytes())
            .await?;
        let hashing = self.workers.turn().await.map_err(Internal::new)?;
        let mut uow = self.database.begin(client).await?;
        let address = take_turn(&mut uow, principal).await?;
        let stored = repo::password_hash(&mut uow, user_id).await?;
        let has_password = stored.is_some();
        let hash = stored.unwrap_or_else(|| self.dummy_hash.clone());
        let ((matches, _), hashing) = verify_in(hashing, hash, password).await?;
        Ok((uow, address, matches && has_password, hashing))
    }
}
