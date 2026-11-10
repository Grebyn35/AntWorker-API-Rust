//! Proving an account's email address with a code sent to it (AUTH-025 to AUTH-033, the auth
//! spec's "Email verification"): whether an account must still prove it, asking for a code, the
//! job that issues a code and sends it, and entering it.

use platform::{
    authn::Principal,
    db::UnitOfWork,
    errors::{Internal, RequestError},
    jobs::{JobContext, JobFailure},
    rate_limit::{LimitError, RateLimits},
    types::{Client, EmailAddress, UserId},
};

use super::Auth;
use crate::modules::{
    accounts::{self, AccountsError, Address},
    audit::{self as audit_log, AuditError, Entry},
    auth::{
        audit,
        emails::CodeEmail,
        error::AuthError,
        jobs::SendCode,
        metrics,
        model::{Purpose, Session, VerificationCode},
        repo::{self, codes},
    },
};

/// How long a code lasts, in minutes: the most that ASVS 5.0.0 gives a code sent out of band
/// (6.5.5), which a code sent by email is held to (AUTH-030).
const CODE_MINUTES: i32 = 10;

/// The kind of the keyed hash of a verification code (SEC-03).
const CODE_KIND: &str = "verification_code";

impl Auth {
    /// Whether `user_id`'s account must still prove its email address before the API serves it
    /// anything but proving it: while email is on and it hasn't (AUTH-025, AUTH-026), or `None`
    /// once the account is gone.
    pub(super) async fn must_verify(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
    ) -> Result<Option<bool>, Internal> {
        if !self.mail.is_on() {
            return Ok(Some(false));
        }
        Ok(address(uow, user_id)
            .await?
            .map(|address| !address.verified))
    }

    /// Asks for a code that proves the caller's address: enqueues the job that issues one and
    /// sends it (AUTH-030), once the request has counted in the bucket of the account's address,
    /// which a read before the unit of work finds (AUTH-028, AUTH-029, ARCH-11). Answers the
    /// address the code goes to.
    pub(crate) async fn request_code(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
    ) -> Result<EmailAddress, AuthError> {
        if !self.mail.is_on() {
            return Err(RequestError::FeatureDisabled.into());
        }
        let mut uow = self.database.begin(client).await?;
        let address = caller_address(&mut uow, principal).await?;
        drop(uow);
        if address.verified {
            return Err(AuthError::EmailAlreadyVerified);
        }
        limits
            .check(
                self.settings.verification_email,
                address.email.as_str().as_bytes(),
            )
            .await?;
        let mut uow = self.database.begin(client).await?;
        self.enqueue_code(&mut uow, principal).await?;
        uow.commit().await?;
        Ok(address.email)
    }

    /// Enqueues in `uow` the job that issues a code to the account of `principal`, and sends it,
    /// while email is on: a registration's (AUTH-027), or a request's (AUTH-028).
    pub(super) async fn enqueue_code(
        &self,
        uow: &mut UnitOfWork,
        principal: &Principal,
    ) -> Result<(), Internal> {
        if !self.mail.is_on() {
            return Ok(());
        }
        uow.enqueue(
            principal.job_scope(),
            &SendCode {
                purpose: Purpose::EmailVerification,
            },
        )
        .await
    }

    /// Proves the caller's address with `code`, and answers their session (AUTH-031, AUTH-032),
    /// once the check has counted in the caller's bucket (AUTH-033), and counts how it ended in
    /// `auth.email_verifications`.
    pub(crate) async fn confirm_code(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        code: VerificationCode,
    ) -> Result<Session, AuthError> {
        if !self.mail.is_on() {
            return Err(RequestError::FeatureDisabled.into());
        }
        let result = self.check_code(limits, client, principal, code).await;
        let outcome = match &result {
            Ok(_) => Some(metrics::Verification::Verified),
            Err(AuthError::WrongCode) => Some(metrics::Verification::WrongCode),
            Err(AuthError::CodeExpired) => Some(metrics::Verification::CodeExpired),
            Err(AuthError::Limit(LimitError::RateLimited)) => {
                Some(metrics::Verification::RateLimited)
            }
            Err(_) => None,
        };
        if let Some(outcome) = outcome {
            metrics::verification(outcome);
        }
        result
    }

    async fn check_code(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        code: VerificationCode,
    ) -> Result<Session, AuthError> {
        let user_id = principal.user_id();
        limits
            .check(self.settings.code_check, user_id.uuid().as_bytes())
            .await?;
        let mut uow = self.database.begin(client).await?;
        // The user's row first, as a deletion of the account locks it first, so the two take
        // turns, and of two checks the second reads the address as the first left it (DATA-20).
        let address = accounts::address_for_update(&mut uow, user_id)
            .await
            .map_err(AuthError::from)?
            .ok_or_else(|| Internal::new("the caller's user doesn't exist"))?;
        if address.verified {
            return Err(AuthError::EmailAlreadyVerified);
        }
        let stored = match codes::find(&mut uow, user_id, Purpose::EmailVerification).await? {
            Some(stored) if !stored.expired => stored,
            _ => return Err(AuthError::CodeExpired),
        };
        let input = code_input(user_id, Purpose::EmailVerification, &code);
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
        accounts::verify_email(&mut uow, user_id).await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::EMAIL_VERIFIED, audit::USER, user_id.uuid()),
        )
        .await?;
        let session = repo::session(&mut uow, user_id, principal.session_id())
            .await?
            .ok_or_else(|| Internal::new("the caller's session doesn't exist"))?;
        uow.commit().await?;
        Ok(session.answer(false))
    }

    /// The run of `auth.send_verification_code` for the user the job runs for: issues a new code,
    /// in place of the one before it, commits its HMAC, and sends it to the account's address at
    /// once, so that the code itself is kept nowhere (AUTH-030, SEC-03). A run for an account that
    /// is gone, or that proved its address meanwhile, or on a replica with email off, issues
    /// nothing, and so does one that another run beat to its code, whose email carries that run's.
    pub(in crate::modules::auth) async fn issue_code(
        &self,
        context: &JobContext,
        job: SendCode,
    ) -> Result<(), JobFailure> {
        if !self.mail.is_on() {
            return Ok(());
        }
        let user_id = context
            .scope()
            .user()
            .ok_or_else(|| JobFailure::Permanent(Internal::new("a code's job runs for no user")))?;
        let scope = context
            .audit_scope()
            .ok_or_else(|| JobFailure::Permanent(Internal::new("a code's job runs for no user")))?;
        let mut uow = self.database.for_job(context).await?;
        let Some(address) = address(&mut uow, user_id).await? else {
            return Ok(());
        };
        if address.verified {
            return Ok(());
        }
        let code = VerificationCode::generate()?;
        let hash = self
            .key
            .hash(CODE_KIND, &code_input(user_id, job.purpose, &code));
        // Rendered before anything is stored, so a template that fails stores no code.
        let email = CodeEmail {
            to: address.email,
            code,
        }
        .render()
        .map_err(JobFailure::Permanent)?;
        let Some(id) = codes::replace(&mut uow, user_id, job.purpose, &hash, CODE_MINUTES).await?
        else {
            return Ok(());
        };
        audit_log::record(
            &mut uow,
            scope,
            Entry::new(
                audit::VERIFICATION_CODE_ISSUED,
                audit::VERIFICATION_CODE,
                id,
            ),
        )
        .await
        .map_err(|AuditError::Internal(error)| error)?;
        uow.commit().await?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the job that makes a code sends it, so no row keeps it (SEC-03)"
        )]
        let sent = self.mail.send_now(email).await;
        sent
    }
}

/// The caller's address, which their session proves exists.
async fn caller_address(uow: &mut UnitOfWork, principal: &Principal) -> Result<Address, AuthError> {
    Ok(address(uow, principal.user_id())
        .await?
        .ok_or_else(|| Internal::new("the caller's user doesn't exist"))?)
}

/// `user_id`'s address, or `None` once the account is gone.
async fn address(uow: &mut UnitOfWork, user_id: UserId) -> Result<Option<Address>, Internal> {
    accounts::address(uow, user_id)
        .await
        .map_err(|error| match error {
            AccountsError::Internal(error) => error,
            other => Internal::new(other),
        })
}

/// What a code's keyed hash covers: its user, its purpose and its digits, so a code proves only
/// its user's purpose, and no two users' rows that hold the same digits hold the same hash.
fn code_input(user_id: UserId, purpose: Purpose, code: &VerificationCode) -> Vec<u8> {
    [
        user_id.uuid().as_bytes().as_slice(),
        purpose.name().as_bytes(),
        &[0],
        code.digits().as_bytes(),
    ]
    .concat()
}
