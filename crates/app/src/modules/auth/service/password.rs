//! Resetting a forgotten password with a code sent to the account's address, and changing the
//! password with the current one (AUTH-034 to AUTH-041, the auth spec's "Password reset" and
//! "Changing the password").

use platform::{
    authn::{AuditScope, Principal},
    db::UnitOfWork,
    errors::{Internal, RequestError},
    events::{self, AccessChange},
    jobs::JobScope,
    rate_limit::{self, LimitError, RateLimits},
    types::{Client, EmailAddress, SessionId, UserId},
};

use super::{
    Auth, buckets, hash,
    verification::{CODE_KIND, code_input},
};
use crate::modules::{
    accounts,
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        emails::PasswordChangedEmail,
        error::AuthError,
        jobs::SendCode,
        metrics,
        model::{Password, Purpose, ResetCode},
        repo::{self, codes},
    },
};

/// A confirmation of a reset, in domain types (AUTH-037).
pub(crate) struct ResetConfirmation {
    pub(crate) email: EmailAddress,
    pub(crate) code: ResetCode,
    pub(crate) password: Password,
}

/// A change of the signed-in user's password, in domain types (AUTH-040).
pub(crate) struct PasswordChange {
    pub(crate) current: Password,
    pub(crate) password: Password,
}

impl Auth {
    /// Asks for a reset of the password of the account that holds `email`: enqueues the job that
    /// issues its reset code and sends it, or the same job for no one when no account holds the
    /// address, so the request does the same work either way (AUTH-034, AUTH-035, SEC-05).
    pub(crate) async fn request_reset(
        &self,
        limits: &RateLimits,
        client: &Client,
        email: EmailAddress,
    ) -> Result<(), AuthError> {
        if !self.mail.is_on() {
            return Err(RequestError::FeatureDisabled.into());
        }
        limits
            .check(buckets::RESET_EMAIL, email.as_str().as_bytes())
            .await?;
        let mut uow = self.database.begin(client).await?;
        let user_id = accounts::find_by_email(&mut uow, &email).await?;
        #[expect(
            clippy::disallowed_methods,
            reason = "a reset enqueues its code's job for the account its address names (PLATFORM-013)"
        )]
        let scope = JobScope::attempt(user_id);
        uow.enqueue(
            scope,
            &SendCode {
                purpose: Purpose::PasswordReset,
            },
        )
        .await?;
        uow.commit().await?;
        Ok(())
    }

    /// Resets the password of the account that holds the confirmation's address with its reset
    /// code (AUTH-037, AUTH-038), once the confirmation has counted in the bucket of the address
    /// from the client's (AUTH-039), and counts how it ended in `auth.password_resets`.
    pub(crate) async fn confirm_reset(
        &self,
        limits: &RateLimits,
        client: &Client,
        confirmation: ResetConfirmation,
    ) -> Result<(), AuthError> {
        if !self.mail.is_on() {
            return Err(RequestError::FeatureDisabled.into());
        }
        let result = self.reset(limits, client, confirmation).await;
        let outcome = match &result {
            Ok(()) => Some(metrics::PasswordReset::Reset),
            Err(AuthError::InvalidResetCode) => Some(metrics::PasswordReset::InvalidResetCode),
            Err(AuthError::Limit(LimitError::RateLimited)) => {
                Some(metrics::PasswordReset::RateLimited)
            }
            Err(_) => None,
        };
        if let Some(outcome) = outcome {
            metrics::reset(outcome);
        }
        result
    }

    async fn reset(
        &self,
        limits: &RateLimits,
        client: &Client,
        confirmation: ResetConfirmation,
    ) -> Result<(), AuthError> {
        let email = confirmation.email.as_str().as_bytes();
        // A zero byte, which no email address holds, ends the email in the key.
        let from_address = [email, &[0], &rate_limit::address(client.ip)].concat();
        limits.check(buckets::RESET_CHECK, &from_address).await?;
        // Hashed before anything is read, so the confirmation does the same work whatever it
        // finds (AUTH-038).
        let password_hash = hash(&self.workers, confirmation.password).await?;
        let mut uow = self.database.begin(client).await?;
        let Some(user_id) = accounts::find_by_email(&mut uow, &confirmation.email).await? else {
            return Err(AuthError::InvalidResetCode);
        };
        // The user's row first, as a deletion of the account locks it first, so the two take
        // turns (DATA-20).
        let Some(address) = accounts::address_for_update(&mut uow, user_id).await? else {
            return Err(AuthError::InvalidResetCode);
        };
        let scope = AuditScope::attempt(user_id);
        if !self
            .claim_reset_code(&mut uow, user_id, &confirmation.code)
            .await?
        {
            // A failed reset of an existing account is a security failure, which commits before
            // the answer (ARCH-11).
            audit_log::record(
                &mut uow,
                scope,
                Entry::new(audit::PASSWORD_RESET_FAILED, audit::USER, user_id.uuid()),
            )
            .await?;
            uow.commit().await?;
            return Err(AuthError::InvalidResetCode);
        }
        let identity = repo::set_password(&mut uow, user_id, &password_hash).await?;
        let ended = repo::delete_sessions(&mut uow, user_id).await?;
        end_sessions(&mut uow, scope, user_id, &ended).await?;
        if !address.verified {
            accounts::verify_email(&mut uow, user_id).await?;
            audit_log::record(
                &mut uow,
                scope,
                Entry::new(audit::EMAIL_VERIFIED, audit::USER, user_id.uuid()),
            )
            .await?;
        }
        audit_log::record(
            &mut uow,
            scope,
            Entry::new(audit::PASSWORD_RESET, audit::IDENTITY, identity),
        )
        .await?;
        #[expect(
            clippy::disallowed_methods,
            reason = "a reset enqueues its email for the account whose code it checked (PLATFORM-013)"
        )]
        let job_scope = JobScope::attempt(Some(user_id));
        self.tell_password_changed(&mut uow, job_scope, address.email)
            .await?;
        uow.commit().await?;
        Ok(())
    }

    /// Whether `code` is `user_id`'s reset code, unexpired, which this unit of work then claimed by
    /// deleting it, so it resets nothing again: a row only one unit of work deletes (AUTH-037).
    async fn claim_reset_code(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        code: &ResetCode,
    ) -> Result<bool, Internal> {
        let Some(stored) = codes::find(uow, user_id, Purpose::PasswordReset).await? else {
            return Ok(false);
        };
        let input = code_input(user_id, Purpose::PasswordReset, code.characters());
        if stored.expired || !self.key.verify(CODE_KIND, &input, &stored.hash) {
            return Ok(false);
        }
        codes::claim(uow, user_id, stored.id).await
    }

    /// Changes the caller's password, once their current one has passed the one check of a
    /// signed-in user's password (AUTH-040, AUTH-022), ending their other sessions.
    pub(crate) async fn change_password(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        change: PasswordChange,
    ) -> Result<(), AuthError> {
        let user_id = principal.user_id();
        let (mut uow, matches) = self
            .check_password(limits, client, user_id, change.current)
            .await?;
        if !matches {
            return Err(self.wrong_password(uow, principal).await?);
        }
        let password_hash = hash(&self.workers, change.password).await?;
        let identity = repo::set_password(&mut uow, user_id, &password_hash).await?;
        let ended = repo::delete_other_sessions(&mut uow, user_id, principal.session_id()).await?;
        end_sessions(&mut uow, principal.audit_scope(), user_id, &ended).await?;
        // The request proved the password, as a re-authentication does.
        repo::reauthenticate(&mut uow, user_id, principal.session_id())
            .await?
            .ok_or_else(|| Internal::new("the caller's session doesn't exist"))?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::PASSWORD_CHANGED, audit::IDENTITY, identity),
        )
        .await?;
        let address = accounts::address(&mut uow, user_id)
            .await?
            .ok_or_else(|| Internal::new("the caller's user doesn't exist"))?;
        self.tell_password_changed(&mut uow, principal.job_scope(), address.email)
            .await?;
        uow.commit().await?;
        Ok(())
    }

    /// Enqueues in `uow` the email that tells `to` its account's password changed, while email is
    /// on (AUTH-041).
    async fn tell_password_changed(
        &self,
        uow: &mut UnitOfWork,
        scope: JobScope,
        to: EmailAddress,
    ) -> Result<(), Internal> {
        if !self.mail.is_on() {
            return Ok(());
        }
        let email = PasswordChangedEmail { to }.render()?;
        self.mail.send(uow, scope, email).await
    }
}

/// Signals that each of `user_id`'s sessions in `ended` ended, so its streams close, and records
/// each as `session.ended` in `scope` (AUTH-037, AUTH-040, D35).
async fn end_sessions(
    uow: &mut UnitOfWork,
    scope: AuditScope,
    user_id: UserId,
    ended: &[SessionId],
) -> Result<(), AuthError> {
    for &session_id in ended {
        events::signal(
            uow,
            AccessChange::SessionEnded {
                user_id,
                session_id,
            },
        )
        .await?;
        audit_log::record(
            uow,
            scope,
            Entry::new(audit::SESSION_ENDED, audit::SESSION, session_id.uuid()),
        )
        .await?;
    }
    Ok(())
}
