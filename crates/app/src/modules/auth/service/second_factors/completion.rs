//! A sign-in's challenge, which a sign-in whose password matched issues for an account with an
//! authenticator app, and its completion with a code (AUTH-058 to AUTH-061).

use platform::{
    authn::AuditScope,
    db::UnitOfWork,
    hashing::sha256,
    random,
    rate_limit::RateLimits,
    types::{Client, Secret, Timestamp, UserId},
};

use super::{
    FactorChange,
    checks::{Checked, count},
};
use crate::modules::{
    accounts,
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        error::AuthError,
        model::{DeviceToken, FactorProof, Session, Tokens},
        repo::{self, second_factors as factors},
        service::{Auth, Device, buckets},
    },
};

/// How long a sign-in's challenge lasts, in minutes (AUTH-058).
const CHALLENGE_MINUTES: i32 = 5;

/// A challenge that a sign-in issued, which a code completes (AUTH-058).
pub(crate) struct Challenge {
    /// Its token, returned once.
    pub(crate) token: Secret<String>,
    /// When it expires.
    pub(crate) expires_at: Timestamp,
}

/// A completion of a sign-in: its challenge's token, and the code that proves the factor
/// (AUTH-059).
pub(crate) struct Completion {
    pub(crate) challenge: Secret<String>,
    pub(crate) proof: FactorProof,
}

impl Auth {
    /// Issues a challenge of `user_id`'s for a sign-in on `device`, whose password matched, with
    /// the hash of the valid device token it brought, and records it, in the sign-in's unit of
    /// work (AUTH-058).
    pub(in crate::modules::auth::service) async fn challenge(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        device: &Device,
        device_token: Option<&[u8; 32]>,
    ) -> Result<Challenge, AuthError> {
        let token = Secret::new(random::token()?);
        let expires_at = factors::insert_challenge(
            uow,
            user_id,
            &sha256(token.expose().as_bytes()),
            (
                device.id.as_ref(),
                device.name.as_ref(),
                device.provider.as_deref(),
            ),
            device_token,
            CHALLENGE_MINUTES,
        )
        .await?;
        let entry = Entry::new(audit::SIGN_IN_CHALLENGED, audit::USER, user_id.uuid());
        let entry = match &device.provider {
            Some(provider) => super::super::providers::through(entry, provider),
            None => entry,
        };
        audit_log::record(uow, AuditScope::attempt(user_id), entry).await?;
        Ok(Challenge { token, expires_at })
    }

    /// Completes the sign-in that `completion`'s challenge waits for, when its code is the user's:
    /// the session starts on the challenge's device, and the device gets its next device token
    /// (AUTH-059 to AUTH-061).
    pub(crate) async fn complete_sign_in(
        &self,
        limits: &RateLimits,
        client: &Client,
        completion: Completion,
    ) -> Result<(Session, Tokens, DeviceToken), AuthError> {
        let token = sha256(completion.challenge.expose().as_bytes());
        // The challenge names the bucket that counts the check, so it is read first, in a unit
        // of work of its own that writes nothing (ARCH-11).
        let mut read = self.database.begin(client).await?;
        let found = factors::challenge(&mut read, &token).await?;
        drop(read);
        let challenge = match found {
            Some(challenge) if !challenge.expired => challenge,
            _ => return Err(AuthError::InvalidChallenge),
        };
        let user_id = challenge.user_id;
        match &challenge.device_token_hash {
            Some(hash) => count(limits, buckets::SECOND_FACTOR_DEVICE, hash).await?,
            None => {
                count(
                    limits,
                    buckets::SECOND_FACTOR_SIGN_IN,
                    user_id.uuid().as_bytes(),
                )
                .await?
            }
        }
        let mut uow = self.database.begin(client).await?;
        // The password identity first, when the account has one, as a sign-in locks it, so the
        // completion takes turns with sign-ins, a reset and a deletion of the account (the auth
        // spec's "Second factors"). A sign-in with the password whose identity a deletion erased
        // meanwhile completes nothing; one through a provider needs no password (AUTH-073).
        let identity = repo::password_identity_for_update(&mut uow, user_id).await?;
        if identity.is_none() && challenge.provider.is_none() {
            return Err(AuthError::InvalidChallenge);
        }
        let used = match self
            .check_factor(
                &mut uow,
                AuditScope::attempt(user_id),
                user_id,
                Some(&completion.proof),
            )
            .await?
        {
            Checked::Passed(used) => used,
            Checked::Wrong => {
                return Err(self
                    .wrong_factor(
                        uow,
                        AuditScope::attempt(user_id),
                        user_id,
                        AuthError::InvalidSecondFactor,
                    )
                    .await?);
            }
            // A removal of the app deleted the challenge too, which the claim below would find.
            Checked::NoApp | Checked::Missing => return Err(AuthError::InvalidChallenge),
        };
        if !factors::claim_challenge(&mut uow, user_id, challenge.id).await? {
            return Err(AuthError::InvalidChallenge);
        }
        self.start_completed(uow, challenge, used).await
    }

    /// Starts the session of `challenge`, whose completion `uow` claimed, with its device token,
    /// and tells the address when a recovery code completed it, leaving `used` (AUTH-059,
    /// AUTH-062).
    async fn start_completed(
        &self,
        mut uow: UnitOfWork,
        challenge: factors::StoredChallenge,
        used: Option<i64>,
    ) -> Result<(Session, Tokens, DeviceToken), AuthError> {
        let user_id = challenge.user_id;
        let address = accounts::address(&mut uow, user_id)
            .await?
            .ok_or(AuthError::InvalidChallenge)?;
        let device = Device {
            id: challenge.device_id,
            name: challenge.device_name,
            provider: challenge.provider,
        };
        let (principal, session, tokens) = self.replace_session(&mut uow, user_id, &device).await?;
        let device = self
            .issue_device_token(
                &mut uow,
                user_id,
                &address.email,
                challenge.device_token_hash.as_ref(),
            )
            .await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::DEVICE_TOKEN_ISSUED, audit::DEVICE_TOKEN, device.id),
        )
        .await?;
        if let Some(left) = used {
            self.tell_factor_changed(
                &mut uow,
                principal.job_scope(),
                address.email,
                FactorChange::RecoveryCodeUsed(left),
            )
            .await?;
        }
        let session = self.answer(&mut uow, session).await?;
        uow.commit().await?;
        Ok((session, tokens, device))
    }
}
