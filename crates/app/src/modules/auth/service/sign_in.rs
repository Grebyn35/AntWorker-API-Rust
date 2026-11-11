//! Signing in with an email and a password: a session, or, for an account with an authenticator
//! app, a challenge that its code completes (AUTH-004 to AUTH-008, AUTH-014, AUTH-058).

use platform::{
    authn::AuditScope,
    db::UnitOfWork,
    rate_limit::{self, LimitError, RateLimits},
    types::{Client, EmailAddress, Secret, UserId},
};

use super::{Auth, Challenge, Device, hash, verify};
use crate::modules::{
    accounts,
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        error::AuthError,
        metrics,
        model::{DeviceToken, Password, Session, Tokens},
        repo,
    },
};

/// A sign-in's input, in domain types (AUTH-004).
pub(crate) struct SignIn {
    pub(crate) email: EmailAddress,
    pub(crate) password: Password,
    pub(crate) device: Device,
    /// The device token the client holds, if any, which counts as none unless it is valid for the
    /// email (AUTH-019).
    pub(crate) device_token: Option<Secret<String>>,
    /// A browser's device tokens, by the names of their cookies after `__Host-`, of which the
    /// sign-in reads its email's when `device_token` is none (AUTH-015).
    pub(crate) device_cookies: Vec<(String, Secret<String>)>,
}

/// How a sign-in whose password matched ends: with a session, or, for an account with an
/// authenticator app, with a challenge that its code completes (AUTH-004, AUTH-058).
pub(crate) enum SignedIn {
    /// The new session, its tokens, and the device's next device token.
    Session(Box<(Session, Tokens, DeviceToken)>),
    /// The challenge.
    Challenged(Challenge),
}

impl Auth {
    /// Signs in with an email and a password (AUTH-004 to AUTH-008), issuing the device a device
    /// token (AUTH-019), or, for an account with an authenticator app, a challenge (AUTH-058), and
    /// counts how the attempt ended in `auth.sign_ins`.
    pub(crate) async fn sign_in(
        &self,
        limits: &RateLimits,
        client: &Client,
        input: SignIn,
    ) -> Result<SignedIn, AuthError> {
        let result = self.attempt(limits, client, input).await;
        match &result {
            Ok(SignedIn::Session(_)) => metrics::sign_in(metrics::SignIn::Succeeded),
            Ok(SignedIn::Challenged(_)) => metrics::sign_in(metrics::SignIn::Challenged),
            Err(AuthError::InvalidCredentials) => {
                metrics::sign_in(metrics::SignIn::InvalidCredentials);
            }
            Err(AuthError::Limit(LimitError::RateLimited)) => {
                metrics::sign_in(metrics::SignIn::RateLimited);
            }
            Err(_) => {}
        }
        result
    }

    async fn attempt(
        &self,
        limits: &RateLimits,
        client: &Client,
        input: SignIn,
    ) -> Result<SignedIn, AuthError> {
        let trusted = self.trusted(client, &input).await?;
        match &trusted {
            // A device token valid for the email counts only in its own limit, so no attempt from
            // elsewhere refuses its sign-in (AUTH-006).
            Some(token) => limits.check(self.settings.sign_in_device, token).await?,
            // Otherwise the email from the client's address first, so the count of the email from
            // every address holds only what that limit let through, and no one address fills it.
            // A zero byte, which no email address holds, ends the email in the first key.
            None => {
                let email = input.email.as_str().as_bytes();
                let from_address = [email, &[0], &rate_limit::address(client.ip)].concat();
                limits.check(self.settings.sign_in, &from_address).await?;
                limits.check(self.settings.sign_in_email, email).await?;
            }
        }
        let mut uow = self.database.begin(client).await?;
        let Some(user_id) = accounts::find_by_email(&mut uow, &input.email).await? else {
            verify(&self.workers, self.dummy_hash.clone(), input.password).await?;
            return Err(AuthError::InvalidCredentials);
        };
        // Locked for the rest of the unit of work, so two sign-ins of one user take turns, and
        // one on a device ends the session the other just started there (DATA-20).
        let Some(identity) = repo::password_identity_for_update(&mut uow, user_id).await? else {
            verify(&self.workers, self.dummy_hash.clone(), input.password).await?;
            return Err(AuthError::InvalidCredentials);
        };
        let (matches, password) =
            verify(&self.workers, identity.hash.clone(), input.password).await?;
        if !matches {
            audit_log::record(
                &mut uow,
                AuditScope::attempt(user_id),
                Entry::new(audit::SIGN_IN_FAILED, audit::USER, user_id.uuid()),
            )
            .await?;
            uow.commit().await?;
            return Err(AuthError::InvalidCredentials);
        }
        // An account with an app gets a challenge, which its code completes, rather than a
        // session; its password, which matched, is rehashed now, since no later step holds it.
        if repo::second_factors::has_app(&mut uow, user_id).await? {
            self.rehash(
                &mut uow,
                AuditScope::attempt(user_id),
                (user_id, identity.id, &identity.hash),
                password,
            )
            .await?;
            let challenge = self
                .challenge(&mut uow, user_id, &input.device, trusted.as_ref())
                .await?;
            uow.commit().await?;
            return Ok(SignedIn::Challenged(challenge));
        }
        let (principal, session, tokens) = self
            .replace_session(&mut uow, user_id, &input.device)
            .await?;
        self.rehash(
            &mut uow,
            principal.audit_scope(),
            (user_id, identity.id, &identity.hash),
            password,
        )
        .await?;
        let device = self
            .issue_device_token(&mut uow, user_id, &input.email, trusted.as_ref())
            .await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::DEVICE_TOKEN_ISSUED, audit::DEVICE_TOKEN, device.id),
        )
        .await?;
        let session = self.answer(&mut uow, session).await?;
        uow.commit().await?;
        Ok(SignedIn::Session(Box::new((session, tokens, device))))
    }

    /// Hashes `password` again, with the current parameters, when the stored hash of the user's
    /// password identity, `identity`'s, was made with others, and records it in the log of
    /// `scope` (AUTH-014).
    async fn rehash(
        &self,
        uow: &mut UnitOfWork,
        scope: AuditScope,
        identity: (UserId, uuid::Uuid, &str),
        password: Password,
    ) -> Result<(), AuthError> {
        let (user_id, identity_id, stored) = identity;
        if stored.starts_with(&self.current_parameters()) {
            return Ok(());
        }
        let rehashed = hash(&self.workers, password).await?;
        repo::update_password_hash(uow, user_id, identity_id, &rehashed).await?;
        audit_log::record(
            uow,
            scope,
            Entry::new(audit::PASSWORD_REHASHED, audit::IDENTITY, identity_id),
        )
        .await?;
        Ok(())
    }
}
