//! The check of a second factor's code: an authenticator app's, or a recovery code, which a
//! completion of a sign-in, a re-authentication and a change of the password go through, with the
//! buckets that count them (AUTH-057, AUTH-061, AUTH-062, AUTH-065, AUTH-066).

use platform::{
    authn::{AuditScope, Principal},
    db::UnitOfWork,
    encryption::{Binding, Sealed},
    errors::{Internal, RequestError},
    hashing::sha256,
    rate_limit::{Bucket, LimitError, RateLimits},
    types::UserId,
};

use super::{APPS, SECRET};
use crate::modules::{
    accounts,
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        emails::FactorChange,
        error::AuthError,
        metrics::{self, FactorCheck},
        model::{AppSecret, FactorProof, RecoveryCode},
        repo::{self, second_factors as factors},
        service::{Auth, buckets},
    },
};

/// What a check of a second factor found.
pub(super) enum Checked {
    /// The account has no app, so no code is asked for.
    NoApp,
    /// The account has an app, and the request gave no code.
    Missing,
    /// The code wasn't the account's.
    Wrong,
    /// The code was the app's, or a recovery code, which left this many.
    Passed(Option<i64>),
}

impl Auth {
    /// The second factor of a signed-in user's check of who they are, a re-authentication's or a
    /// change of the password's, once their password matched in `uow`: nothing for an account
    /// without an app, or else `proof`, which must be the app's code or a recovery code. A wrong
    /// one is recorded in the caller's log, whose unit of work commits before the answer
    /// (AUTH-065, ARCH-11). Its bucket counted the request before `uow` opened (AUTH-061). It
    /// locks the password identity, then the app, the order of every operation that locks both
    /// (DATA-20).
    pub(in crate::modules::auth::service) async fn check_signed_in_factor(
        &self,
        mut uow: UnitOfWork,
        principal: &Principal,
        proof: Option<&FactorProof>,
    ) -> Result<UnitOfWork, AuthError> {
        let user_id = principal.user_id();
        // The password identity before the app, as a sign-in's completion locks them, since a
        // change of the password sets the identity's hash after this check (the auth spec's
        // "Second factors").
        repo::password_identity_for_update(&mut uow, user_id).await?;
        match self
            .check_factor(&mut uow, principal.audit_scope(), user_id, proof)
            .await?
        {
            Checked::NoApp => Ok(uow),
            Checked::Missing => Err(AuthError::SecondFactorRequired),
            Checked::Wrong => Err(self
                .wrong_factor(
                    uow,
                    principal.audit_scope(),
                    user_id,
                    AuthError::WrongSecondFactor,
                )
                .await?),
            Checked::Passed(used) => {
                if let Some(left) = used {
                    let address = accounts::address(&mut uow, user_id)
                        .await?
                        .ok_or(RequestError::Unauthenticated)?;
                    self.tell_factor_changed(
                        &mut uow,
                        principal.job_scope(),
                        address.email,
                        FactorChange::RecoveryCodeUsed(left),
                    )
                    .await?;
                }
                Ok(uow)
            }
        }
    }

    /// Counts a signed-in user's check of a second factor in their bucket, before its unit of
    /// work opens, when the request gives a code (AUTH-061).
    pub(in crate::modules::auth::service) async fn count_signed_in_check(
        &self,
        limits: &RateLimits,
        principal: &Principal,
        proof: Option<&FactorProof>,
    ) -> Result<(), AuthError> {
        if proof.is_some() {
            count(
                limits,
                buckets::SECOND_FACTOR_CHECK,
                principal.user_id().uuid().as_bytes(),
            )
            .await?;
        }
        Ok(())
    }

    /// Checks `proof` against `user_id`'s app, which it locks `FOR UPDATE`, so two checks take
    /// turns, recording a recovery code's use in the log of `scope`: the app's code of a step it
    /// hasn't accepted yet, which it records, storing the
    /// secret anew under the key ring's current version when an older one holds it, or one of
    /// the user's recovery codes, which it deletes (AUTH-057, AUTH-062, AUTH-066).
    pub(super) async fn check_factor(
        &self,
        uow: &mut UnitOfWork,
        scope: AuditScope,
        user_id: UserId,
        proof: Option<&FactorProof>,
    ) -> Result<Checked, AuthError> {
        let Some(app) = factors::app_for_update(uow, user_id).await? else {
            return Ok(Checked::NoApp);
        };
        let Some(proof) = proof else {
            return Ok(Checked::Missing);
        };
        let checked = match proof {
            FactorProof::App(code) => {
                let binding = Binding::new(APPS, SECRET, app.id);
                let secret =
                    AppSecret::from_bytes(self.keyring.open(&binding, &app.secret)?.expose())?;
                let now = factors::now(uow).await?;
                match secret.accepts(code, now, Some(app.last_used_step)) {
                    Some(step) => {
                        let resealed = self.reseal(&binding, &app.secret, &secret)?;
                        factors::record_step(uow, user_id, app.id, step, resealed.as_ref()).await?;
                        Checked::Passed(None)
                    }
                    None => Checked::Wrong,
                }
            }
            FactorProof::Recovery(code) => {
                // Found by its hash, as a token is, and claimed by its deletion, which only one
                // unit of work makes (SEC-03).
                match factors::claim_recovery_code(uow, user_id, &recovery_hash(user_id, code))
                    .await?
                {
                    Some(id) => {
                        audit_log::record(
                            uow,
                            scope,
                            Entry::new(audit::RECOVERY_CODE_USED, audit::RECOVERY_CODE, id),
                        )
                        .await?;
                        Checked::Passed(Some(factors::count_recovery_codes(uow, user_id).await?))
                    }
                    None => Checked::Wrong,
                }
            }
        };
        metrics::factor_check(match checked {
            Checked::Wrong => FactorCheck::WrongCode,
            _ => FactorCheck::Passed,
        });
        Ok(checked)
    }

    /// `secret`, sealed anew under the key ring's current version, when `stored` is sealed under
    /// another (AUTH-066).
    fn reseal(
        &self,
        binding: &Binding,
        stored: &Sealed,
        secret: &AppSecret,
    ) -> Result<Option<Sealed>, Internal> {
        if stored.version == self.keyring.current() {
            return Ok(None);
        }
        self.keyring.seal(binding, secret.bytes()).map(Some)
    }

    /// Records in `uow` that a second factor's code was wrong, in the log of `scope`, a security
    /// failure, which commits before the answer, and returns `answer` (AUTH-056, AUTH-060,
    /// AUTH-065, ARCH-11).
    pub(super) async fn wrong_factor(
        &self,
        mut uow: UnitOfWork,
        scope: AuditScope,
        user_id: UserId,
        answer: AuthError,
    ) -> Result<AuthError, AuthError> {
        audit_log::record(
            &mut uow,
            scope,
            Entry::new(
                audit::SECOND_FACTOR_CHECK_FAILED,
                audit::USER,
                user_id.uuid(),
            ),
        )
        .await?;
        uow.commit().await?;
        Ok(answer)
    }
}

/// The SHA-256 hash that stands in for a recovery code: of its user's ID and its characters, so
/// it holds for that user alone (AUTH-062, SEC-03).
pub(super) fn recovery_hash(user_id: UserId, code: &RecoveryCode) -> [u8; 32] {
    sha256(
        &[
            user_id.uuid().as_bytes().as_slice(),
            code.characters().as_bytes(),
        ]
        .concat(),
    )
}

/// Counts the request in `bucket`, under `key`, and counts a refusal in the second factors'
/// checks (AUTH-061).
pub(super) async fn count(
    limits: &RateLimits,
    bucket: Bucket,
    key: &[u8],
) -> Result<(), AuthError> {
    limits.check(bucket, key).await.map_err(|error| {
        if matches!(error, LimitError::RateLimited) {
            metrics::factor_check(FactorCheck::RateLimited);
        }
        AuthError::from(error)
    })
}
