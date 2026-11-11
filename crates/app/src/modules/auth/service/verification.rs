//! Proving an account's email address with a code sent to it (AUTH-025 to AUTH-033, the auth
//! spec's "Email verification"): whether an account must still prove it, asking for a code, the
//! job that issues a code and sends it, a verification code, a reset code (AUTH-036) or a change
//! code (AUTH-050), and entering a verification code.

use platform::{
    authn::Principal,
    db::UnitOfWork,
    errors::{Internal, RequestError},
    jobs::{JobContext, JobFailure},
    mail::Email,
    rate_limit::{LimitError, RateLimits},
    types::{Client, EmailAddress, UserId},
};

use super::{Auth, buckets};
use crate::modules::{
    accounts::{self, AccountsError, Address},
    audit::{self as audit_log, Action, AuditError, Entry},
    auth::{
        audit,
        emails::{ChangeCodeEmail, CodeEmail, ResetCodeEmail},
        error::AuthError,
        jobs::SendCode,
        metrics,
        model::{ChangeCode, Code, Purpose, ResetCode, Session, VerificationCode},
        repo::{self, codes},
    },
};

/// How long a code lasts, in minutes: the most that ASVS 5.0.0 gives a code sent out of band
/// (6.5.5), which a code sent by email is held to (AUTH-030, AUTH-036, AUTH-050).
pub(super) const CODE_MINUTES: i32 = 10;

/// The kind of the keyed hash of a verification code, a reset code or a change code (SEC-03).
pub(super) const CODE_KIND: &str = "verification_code";

impl Auth {
    /// Whether `user_id`'s account must still prove its email address before the API serves it
    /// anything but proving it: while email is on and it hasn't (AUTH-025, AUTH-026), or `None`
    /// once the account is gone.
    pub(super) async fn email_verification_required(
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
                buckets::VERIFICATION_EMAIL,
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
                email: None,
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
            .check(buckets::CODE_CHECK, user_id.uuid().as_bytes())
            .await?;
        let mut uow = self.database.begin(client).await?;
        // The user's row first, as a deletion of the account locks it first, so the two take
        // turns, and of two checks the second reads the address as the first left it (DATA-20).
        // A deletion of the account that committed meanwhile took the session with the user.
        let address = accounts::address_for_update(&mut uow, user_id)
            .await
            .map_err(AuthError::from)?
            .ok_or(RequestError::Unauthenticated)?;
        if address.verified {
            return Err(AuthError::EmailAlreadyVerified);
        }
        let stored = match codes::find(&mut uow, user_id, Purpose::EmailVerification)
            .await?
            .pop()
        {
            Some(stored) if !stored.expired => stored,
            _ => return Err(AuthError::CodeExpired),
        };
        let input = code_input(user_id, &address.email, &code);
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
        // Another request may have ended the session since it authenticated this one, which then
        // proves nothing, as a request sent after the end (AUTH-016).
        let session = repo::session(&mut uow, user_id, principal.session_id())
            .await?
            .ok_or(RequestError::Unauthenticated)?;
        let session = self.answer(&mut uow, session).await?;
        uow.commit().await?;
        Ok(session)
    }

    /// The run of `auth.send_verification_code` for the user the job runs for: issues a new code
    /// of the job's purpose, a verification code in place of the one before it, a reset code
    /// beside any other, or a change code in place of the one before it, commits its HMAC, and
    /// sends it at once, to the account's address, or to the address a change asked for, so that
    /// the code itself is kept nowhere (AUTH-030, AUTH-036, AUTH-050, SEC-03). A run for no one,
    /// as a reset's for an address that no account holds, or for an account that is gone, or on
    /// a replica with email off, issues nothing; so does a run of a verification code for an
    /// account that proved its address meanwhile, and one that another run beat to its code,
    /// whose email carries that run's.
    pub(in crate::modules::auth) async fn issue_code(
        &self,
        context: &JobContext,
        job: SendCode,
    ) -> Result<(), JobFailure> {
        if !self.mail.is_on() {
            return Ok(());
        }
        let (Some(user_id), Some(scope)) = (context.scope().user(), context.audit_scope()) else {
            return Ok(());
        };
        let mut uow = self.database.for_job(context).await?;
        let Some(address) = address(&mut uow, user_id).await? else {
            return Ok(());
        };
        let Some((id, email, issued)) = self.issue(&mut uow, user_id, address, job).await? else {
            return Ok(());
        };
        audit_log::record(
            &mut uow,
            scope,
            Entry::new(issued, audit::VERIFICATION_CODE, id),
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

    /// Issues `user_id`'s new code of `job`'s purpose in `uow`, and renders its email, which
    /// sends it to `address`, the account's, or to the address a change asked for; or `None` when
    /// the run issues nothing, as for a verification code of an account that proved its address,
    /// or one that another run beat to its code (AUTH-030, AUTH-036, AUTH-050).
    async fn issue(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        address: Address,
        job: SendCode,
    ) -> Result<Option<(uuid::Uuid, Email, Action)>, JobFailure> {
        // Rendered before anything is stored, so a template that fails stores no code.
        let issued = match job.purpose {
            Purpose::EmailVerification => {
                if address.verified {
                    return Ok(None);
                }
                let code = VerificationCode::generate()?;
                let hash = self
                    .key
                    .hash(CODE_KIND, &code_input(user_id, &address.email, &code));
                let email = CodeEmail {
                    to: address.email,
                    code,
                }
                .render()
                .map_err(JobFailure::Permanent)?;
                let purpose = Purpose::EmailVerification;
                let Some(id) = codes::replace(uow, user_id, purpose, &hash, CODE_MINUTES).await?
                else {
                    return Ok(None);
                };
                (id, email, audit::VERIFICATION_CODE_ISSUED)
            }
            Purpose::PasswordReset => {
                let code = ResetCode::generate()?;
                let hash = self
                    .key
                    .hash(CODE_KIND, &code_input(user_id, &address.email, &code));
                let email = ResetCodeEmail {
                    to: address.email,
                    code,
                }
                .render()
                .map_err(JobFailure::Permanent)?;
                let id = codes::insert_reset(uow, user_id, &hash, CODE_MINUTES).await?;
                (id, email, audit::PASSWORD_RESET_CODE_ISSUED)
            }
            Purpose::EmailChange => {
                let to = job.email.ok_or_else(|| {
                    JobFailure::Permanent(Internal::new("a change's job names no address"))
                })?;
                let code = ChangeCode::generate()?;
                let hash = self.key.hash(CODE_KIND, &code_input(user_id, &to, &code));
                let email = ChangeCodeEmail { to, code }
                    .render()
                    .map_err(JobFailure::Permanent)?;
                let purpose = Purpose::EmailChange;
                let Some(id) = codes::replace(uow, user_id, purpose, &hash, CODE_MINUTES).await?
                else {
                    return Ok(None);
                };
                (id, email, audit::EMAIL_CHANGE_CODE_ISSUED)
            }
        };
        Ok(Some(issued))
    }
}

/// The caller's address, which their session proved exists; once a deletion of the account has
/// taken it, and the session with it, the request answers as one sent after does (ACCOUNTS-019).
async fn caller_address(uow: &mut UnitOfWork, principal: &Principal) -> Result<Address, AuthError> {
    Ok(address(uow, principal.user_id())
        .await?
        .ok_or(RequestError::Unauthenticated)?)
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

/// What a code's keyed hash covers: its user, its purpose, the address it was sent to and its
/// characters, so a code proves only its user's purpose for that address, and nothing once the
/// account's address is another, and no two users' rows that hold the same characters hold the
/// same hash (the auth spec's "Changing the email address").
pub(super) fn code_input<C: Code>(user_id: UserId, address: &EmailAddress, code: &C) -> Vec<u8> {
    // A zero byte, which neither a purpose's name nor an address holds, ends each.
    [
        user_id.uuid().as_bytes().as_slice(),
        C::PURPOSE.name().as_bytes(),
        &[0],
        address.as_str().as_bytes(),
        &[0],
        code.characters().as_bytes(),
    ]
    .concat()
}
