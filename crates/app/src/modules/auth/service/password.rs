//! Resetting a forgotten password with a code sent to the account's address, and changing the
//! password with the current one (AUTH-034 to AUTH-041, the auth spec's "Password reset" and
//! "Changing the password").

use platform::{
    authn::{AuditScope, Principal},
    db::UnitOfWork,
    errors::{Internal, RequestError},
    jobs::JobScope,
    rate_limit::{self, LimitError, RateLimits},
    types::{Client, EmailAddress, UserId},
};

use super::{
    Auth, buckets, hash, hash_in,
    sessions::end_sessions,
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
        model::{FactorProof, Password, Purpose, ResetCode},
        repo::{self, codes},
    },
};

/// A confirmation of a reset, in domain types (AUTH-037).
pub(crate) struct ResetConfirmation {
    pub(crate) email: EmailAddress,
    pub(crate) code: ResetCode,
    pub(crate) password: Password,
    /// The second factor's code, which an account with an authenticator app gives (AUTH-037).
    pub(crate) proof: Option<FactorProof>,
}

/// A change of the signed-in user's password, in domain types (AUTH-040).
pub(crate) struct PasswordChange {
    pub(crate) current: Password,
    pub(crate) password: Password,
    /// The second factor's code, which an account with an authenticator app gives (AUTH-065).
    pub(crate) proof: Option<FactorProof>,
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
                email: None,
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
        // Hashed before anything is read, so the confirmation costs the same hash whatever it
        // finds (AUTH-038).
        let password_hash = hash(&self.workers, confirmation.password).await?;
        if confirmation.proof.is_some() {
            self.count_reset_factor(limits, client, &confirmation.email, &confirmation.code)
                .await?;
        }
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
        #[expect(
            clippy::disallowed_methods,
            reason = "a reset enqueues its emails for the account whose code it checked (PLATFORM-013)"
        )]
        let job_scope = JobScope::attempt(Some(user_id));
        let Some(code) = self
            .matching_reset_code(&mut uow, user_id, &address.email, &confirmation.code)
            .await?
        else {
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
        };
        // The app's code, or a recovery code, of an account with an app, once the reset code
        // matched, so whoever reads the mailbox alone resets nothing; a refusal leaves the reset
        // codes as they were (D136).
        let mut uow = self
            .check_reset_factor(
                uow,
                user_id,
                (&address.email, job_scope),
                confirmation.proof.as_ref(),
            )
            .await?;
        // The job of another request's code deletes the codes that expired, holding no turn of
        // the user's, so a code that expired while the check ran may be gone: the reset then
        // rolls back whole, the second factor's check with it (AUTH-037).
        if !codes::claim(&mut uow, user_id, code).await? {
            return Err(AuthError::InvalidResetCode);
        }
        codes::delete_resets(&mut uow, user_id).await?;
        let identity = repo::set_password(&mut uow, user_id, &password_hash).await?;
        // No challenge issued for the password before completes a sign-in (AUTH-069).
        repo::second_factors::delete_challenges(&mut uow, user_id).await?;
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
        self.tell_password_changed(&mut uow, job_scope, address.email)
            .await?;
        uow.commit().await?;
        Ok(())
    }

    /// Counts a confirmation's check of a second factor, which it gives, once its reset code
    /// matched, in the bucket of the user it names: a read before the unit of work opens, in one
    /// of its own that writes nothing, finds the user and whether the code matches, so a reset
    /// code that doesn't match counts in no user's bucket, and whoever knows the address alone
    /// fills none (AUTH-037, AUTH-061, ARCH-11).
    async fn count_reset_factor(
        &self,
        limits: &RateLimits,
        client: &Client,
        email: &EmailAddress,
        code: &ResetCode,
    ) -> Result<(), AuthError> {
        let mut read = self.database.begin(client).await?;
        let mut matched = None;
        if let Some(user_id) = accounts::find_by_email(&mut read, email).await?
            && let Some(address) = accounts::address(&mut read, user_id).await?
            && self
                .matching_reset_code(&mut read, user_id, &address.email, code)
                .await?
                .is_some()
        {
            matched = Some(user_id);
        }
        drop(read);
        match matched {
            Some(user_id) => self.count_reset_check(limits, user_id).await,
            None => Ok(()),
        }
    }

    /// The ID of `user_id`'s reset code that lasts and is `code`, sent to `address`, their
    /// address, if any (AUTH-037, AUTH-038). A unit of work claims it by deleting it, so it resets
    /// nothing again: a row only one unit of work deletes.
    async fn matching_reset_code(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        address: &EmailAddress,
        code: &ResetCode,
    ) -> Result<Option<uuid::Uuid>, Internal> {
        let input = code_input(user_id, address, code);
        let stored = codes::find(uow, user_id, Purpose::PasswordReset).await?;
        // Each lasting code is compared, in constant time, so the time doesn't tell which matched.
        let mut matched = None;
        for stored in &stored {
            if self.key.verify(CODE_KIND, &input, &stored.hash) && !stored.expired {
                matched = Some(stored.id);
            }
        }
        Ok(matched)
    }

    /// Changes the caller's password, once their current one has passed the one check of a
    /// signed-in user's password (AUTH-040, AUTH-022), ending their other sessions at the turn at
    /// ending their sessions that the check took, so a request that ended this one's session first
    /// leaves it 401.
    pub(crate) async fn change_password(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        change: PasswordChange,
    ) -> Result<(), AuthError> {
        let user_id = principal.user_id();
        self.count_signed_in_check(limits, principal, change.proof.as_ref())
            .await?;
        // The check takes the user's turn, so a change that came first has set its hash, which
        // this check reads (AUTH-022, DATA-20).
        let (uow, address, matches, hashing) = self
            .check_password(limits, client, principal, change.current)
            .await?;
        if !matches {
            return Err(self.wrong_password(uow, principal).await?);
        }
        let mut uow = self
            .check_signed_in_factor(uow, principal, change.proof.as_ref())
            .await?;
        // In the turn at hashing that the check took before the unit of work opened, which goes
        // before the change's writes (AUTH-093).
        let password_hash = hash_in(&hashing, change.password).await?;
        drop(hashing);
        // The identity before any session, as a sign-in locks them, so the two never deadlock
        // (the auth spec's "Taking turns").
        let identity = repo::set_password(&mut uow, user_id, &password_hash).await?;
        // No challenge issued for the password before completes a sign-in (AUTH-069).
        repo::second_factors::delete_challenges(&mut uow, user_id).await?;
        // The request proved the password, as a re-authentication does; its session, read after
        // the turn, may have ended meanwhile.
        if repo::reauthenticate(&mut uow, user_id, principal.session_id())
            .await?
            .is_none()
        {
            return Err(RequestError::Unauthenticated.into());
        }
        let ended = repo::delete_other_sessions(&mut uow, user_id, principal.session_id()).await?;
        end_sessions(&mut uow, principal.audit_scope(), user_id, &ended).await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::PASSWORD_CHANGED, audit::IDENTITY, identity),
        )
        .await?;
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
