//! Re-authenticating a session with its user's password, and the one check of a signed-in user's
//! password (AUTH-021, AUTH-022, the auth spec's "Re-authentication").

use platform::{
    authn::Principal,
    errors::Internal,
    rate_limit::RateLimits,
    types::{Client, UserId},
};

use super::{Auth, verify};
use crate::modules::{
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        error::AuthError,
        model::{Password, Session},
        repo,
    },
};

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
        let matches = self
            .check_password(limits, client, user_id, password)
            .await?;
        let mut uow = self.database.begin(client).await?;
        if !matches {
            audit_log::record(
                &mut uow,
                principal.audit_scope(),
                Entry::new(audit::PASSWORD_CHECK_FAILED, audit::USER, user_id.uuid()),
            )
            .await?;
            uow.commit().await?;
            return Err(AuthError::WrongPassword);
        }
        let session = repo::reauthenticate(&mut uow, user_id, principal.session_id())
            .await?
            .ok_or_else(|| Internal::new("the caller's session doesn't exist"))?;
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
        uow.commit().await?;
        Ok(session)
    }

    /// Whether `password` is `user_id`'s: the one check of a signed-in user's password, which
    /// counts in their bucket first, reads their password identity in a unit of work that writes
    /// nothing, and verifies the password through `workers` with no connection held, against the
    /// dummy hash when they have none, so a missing password costs what a wrong one does
    /// (AUTH-021, AUTH-022, SEC-13).
    async fn check_password(
        &self,
        limits: &RateLimits,
        client: &Client,
        user_id: UserId,
        password: Password,
    ) -> Result<bool, AuthError> {
        limits
            .check(self.settings.password_check, user_id.uuid().as_bytes())
            .await?;
        let stored = {
            let mut uow = self.database.begin(client).await?;
            repo::password_hash(&mut uow, user_id).await?
        };
        let has_password = stored.is_some();
        let hash = stored.unwrap_or_else(|| self.dummy_hash.clone());
        let (matches, _) = verify(&self.workers, hash, password).await?;
        Ok(matches && has_password)
    }
}
