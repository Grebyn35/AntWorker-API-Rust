//! Changing the account's email address to another, which a code sent to the new address proves
//! (AUTH-048 to AUTH-053, the auth spec's "Changing the email address"): asking, once the session
//! has proved its password just before, which enqueues the job that issues the code, and
//! confirming with the code. The job's run is in `verification.rs`, beside the other codes'.

use platform::{
    authn::Principal,
    db::UnitOfWork,
    errors::{Internal, RequestError},
    jobs::JobScope,
    rate_limit::{LimitError, RateLimits},
    types::{Client, EmailAddress},
};

use super::{
    Auth, buckets,
    reauthentication::require_recent_proof,
    verification::{CODE_KIND, code_input},
};
use crate::modules::{
    accounts::{self, EmailChange},
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        emails::EmailChangedEmail,
        error::AuthError,
        jobs::SendCode,
        metrics,
        model::{ChangeCode, Purpose, Session},
        repo::{self, codes},
    },
};

/// A confirmation of a change of the caller's address, in domain types (AUTH-051).
pub(crate) struct ChangeConfirmation {
    /// The address the change asked for, which the code was sent to.
    pub(crate) email: EmailAddress,
    pub(crate) code: ChangeCode,
}

impl Auth {
    /// Asks to change the caller's address to `email`: enqueues the job that issues the code that
    /// changes it, and sends it there (AUTH-050), once the request has counted in the bucket of
    /// the address (AUTH-049), and the session proved its password within the last 5 minutes
    /// (AUTH-048). Answers the address the code goes to.
    pub(crate) async fn request_email_change(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        email: EmailAddress,
    ) -> Result<EmailAddress, AuthError> {
        if !self.mail.is_on() {
            return Err(RequestError::FeatureDisabled.into());
        }
        limits
            .check(buckets::CHANGE_EMAIL, email.as_str().as_bytes())
            .await?;
        let mut uow = self.database.begin(client).await?;
        require_recent_proof(&mut uow, principal).await?;
        // An address that an account holds, the caller's own among them, is the enumeration that
        // SEC-05 accepts, behind the proof.
        if accounts::find_by_email(&mut uow, &email).await?.is_some() {
            return Err(AuthError::EmailInUse);
        }
        uow.enqueue(
            principal.job_scope(),
            &SendCode {
                purpose: Purpose::EmailChange,
                email: Some(email.clone()),
            },
        )
        .await?;
        uow.commit().await?;
        Ok(email)
    }

    /// Changes the caller's address to the confirmation's, which its code proves, and answers
    /// their session (AUTH-051, AUTH-052), once the check has counted in the caller's bucket of
    /// code checks (AUTH-033), and counts how it ended in `auth.email_changes`.
    pub(crate) async fn confirm_email_change(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        confirmation: ChangeConfirmation,
    ) -> Result<Session, AuthError> {
        if !self.mail.is_on() {
            return Err(RequestError::FeatureDisabled.into());
        }
        let result = self
            .change_email(limits, client, principal, confirmation)
            .await;
        let outcome = match &result {
            Ok(_) => Some(metrics::EmailChange::Changed),
            Err(AuthError::WrongCode) => Some(metrics::EmailChange::WrongCode),
            Err(AuthError::CodeExpired) => Some(metrics::EmailChange::CodeExpired),
            Err(AuthError::EmailInUse) => Some(metrics::EmailChange::EmailInUse),
            Err(AuthError::Limit(LimitError::RateLimited)) => {
                Some(metrics::EmailChange::RateLimited)
            }
            Err(_) => None,
        };
        if let Some(outcome) = outcome {
            metrics::email_change(outcome);
        }
        result
    }

    async fn change_email(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        confirmation: ChangeConfirmation,
    ) -> Result<Session, AuthError> {
        let user_id = principal.user_id();
        limits
            .check(buckets::CODE_CHECK, user_id.uuid().as_bytes())
            .await?;
        let mut uow = self.database.begin(client).await?;
        // The user's row first, as a deletion of the account and a reset lock it first, so they
        // take turns, and of two confirmations the second finds the code the first claimed gone
        // (DATA-20). A deletion that committed meanwhile took the session with the user.
        let before = accounts::address_for_update(&mut uow, user_id)
            .await?
            .ok_or(RequestError::Unauthenticated)?;
        let stored = match codes::find(&mut uow, user_id, Purpose::EmailChange)
            .await?
            .pop()
        {
            Some(stored) if !stored.expired => stored,
            _ => return Err(AuthError::CodeExpired),
        };
        let input = code_input(user_id, &confirmation.email, &confirmation.code);
        if !self.key.verify(CODE_KIND, &input, &stored.hash) {
            // A wrong code is a security failure, which commits before the answer (ARCH-11).
            audit_log::record(
                &mut uow,
                principal.audit_scope(),
                Entry::new(
                    audit::VERIFICATION_CODE_CHECK_FAILED,
                    audit::VERIFICATION_CODE,
                    stored.id,
                ),
            )
            .await?;
            uow.commit().await?;
            return Err(AuthError::WrongCode);
        }
        // A newer code replaced it while the check ran, which its job's run, which holds no lock
        // on the user's row, may do.
        if !codes::claim(&mut uow, user_id, stored.id).await? {
            return Err(AuthError::CodeExpired);
        }
        // Every other code went to the address before. They go before the user's row is held
        // `FOR UPDATE`, which a code's job's insert waits for once it has deleted the code before
        // it, so the two never deadlock (DATA-20).
        codes::delete_all(&mut uow, user_id).await?;
        // The unique index decides whether another account took the address meanwhile: its
        // refusal aborts the unit of work, which rolls back as it is dropped, the claim with it.
        if accounts::change_email(&mut uow, principal, &confirmation.email).await?
            == EmailChange::InUse
        {
            return Err(AuthError::EmailInUse);
        }
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::EMAIL_CHANGED, audit::USER, user_id.uuid()),
        )
        .await?;
        if before.verified {
            self.tell_email_changed(
                &mut uow,
                principal.job_scope(),
                before.email,
                confirmation.email,
            )
            .await?;
        }
        // Another request may have ended the session since it authenticated this one, which then
        // changes nothing, as a request sent after the end (AUTH-016).
        let session = repo::session(&mut uow, user_id, principal.session_id())
            .await?
            .ok_or(RequestError::Unauthenticated)?;
        let session = self.answer(&mut uow, session).await?;
        uow.commit().await?;
        Ok(session)
    }

    /// Enqueues in `uow` the email that tells `to`, the address the account had proved before a
    /// change, that its address is `new` now (AUTH-053). Email is on, as the change needs.
    async fn tell_email_changed(
        &self,
        uow: &mut UnitOfWork,
        scope: JobScope,
        to: EmailAddress,
        new: EmailAddress,
    ) -> Result<(), Internal> {
        let email = EmailChangedEmail { to, new }.render()?;
        self.mail.send(uow, scope, email).await
    }
}
