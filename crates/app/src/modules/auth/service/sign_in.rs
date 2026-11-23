//! Signing in with an email and a password: a session, or, for an account with an authenticator
//! app, a challenge that its code completes (AUTH-004 to AUTH-008, AUTH-014, AUTH-058).

use platform::{
    authn::AuditScope,
    blocking::Turn,
    db::UnitOfWork,
    errors::Internal,
    rate_limit::{self, LimitError, RateLimits},
    types::{Client, EmailAddress, Secret, UserId},
};

use super::{Auth, Challenge, Device, hash_in, verify_in};
use crate::modules::{
    accounts,
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        error::AuthError,
        metrics,
        model::{DeviceToken, Password, Session, Tokens},
        repo::{self, StoredPassword},
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
        // The turn at hashing before the unit of work, so that a sign-in that waits for it holds
        // no connection of the pool, which the other requests need (AUTH-093).
        let hashing = self.workers.turn().await.map_err(Internal::new)?;
        let mut uow = self.database.begin(client, None).await?;
        let user_id = accounts::find_by_email(&mut uow, &input.email).await?;
        // Read without a lock, so a wrong password waits for no other sign-in's lock, and read
        // for an email that no account holds too, so the two run the same reads (SEC-05).
        let read = repo::stored_password(&mut uow, user_id).await?;
        let Some(user_id) = user_id else {
            verify_in(hashing, self.dummy_hash.clone(), input.password).await?;
            return Err(AuthError::InvalidCredentials);
        };
        let Some(read) = read else {
            verify_in(hashing, self.dummy_hash.clone(), input.password).await?;
            return Err(failed(uow, user_id).await?);
        };
        let ((matches, password), hashing) =
            verify_in(hashing, read.hash.clone(), input.password).await?;
        if !matches {
            drop(hashing);
            return Err(failed(uow, user_id).await?);
        }
        let Some((identity, password, hashing)) =
            locked(&mut uow, user_id, &read.hash, (password, hashing)).await?
        else {
            return Err(failed(uow, user_id).await?);
        };
        // A password that matched is hashed again here, if its hash needs it, since no later
        // step holds the password, and the turn goes before the sign-in's writes, so the next
        // sign-in hashes while this one writes (AUTH-014, AUTH-093).
        let rehashed = self.rehashed(hashing, &identity.hash, password).await?;
        // An account with an app gets a challenge, which its code completes, rather than a
        // session; its password's new hash is stored now, since no later step holds it.
        if repo::second_factors::has_app(&mut uow, user_id).await? {
            self.store_rehashed(
                &mut uow,
                AuditScope::attempt(user_id),
                (user_id, identity.id),
                rehashed,
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
        self.store_rehashed(
            &mut uow,
            principal.audit_scope(),
            (user_id, identity.id),
            rehashed,
        )
        .await?;
        let device = self
            .issue_device_token(&mut uow, user_id, trusted.as_ref())
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

    /// The hash of `password` with the current parameters, made in the sign-in's turn at
    /// hashing, which goes with it, when `stored`, the hash of the user's password identity, was
    /// made with others (AUTH-014).
    async fn rehashed(
        &self,
        hashing: Turn,
        stored: &str,
        password: Password,
    ) -> Result<Option<String>, AuthError> {
        if stored.starts_with(&self.current_parameters()) {
            return Ok(None);
        }
        let rehashed = hash_in(hashing, password).await?;
        Ok(Some(rehashed))
    }

    /// Stores `rehashed`, if the sign-in made one, as the hash of the user's password identity,
    /// `identity`, and records it in the log of `scope` (AUTH-014).
    async fn store_rehashed(
        &self,
        uow: &mut UnitOfWork,
        scope: AuditScope,
        identity: (UserId, uuid::Uuid),
        rehashed: Option<String>,
    ) -> Result<(), AuthError> {
        let (user_id, identity_id) = identity;
        let Some(rehashed) = rehashed else {
            return Ok(());
        };
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

/// Records in `uow` that a sign-in to `user_id`'s account failed, with a wrong password or with
/// any password for an account that has none, a security failure, which commits before the
/// answer, and returns the answer (AUTH-005, DATA-22, ARCH-11). An account that a deletion took
/// before the record has no log left to hold it, and the sign-in answers as for an email that no
/// account holds.
async fn failed(mut uow: UnitOfWork, user_id: UserId) -> Result<AuthError, AuthError> {
    let entry = Entry::new(audit::SIGN_IN_FAILED, audit::USER, user_id.uuid());
    if audit_log::record_unless_log_gone(&mut uow, AuditScope::attempt(user_id), entry).await? {
        uow.commit().await?;
    }
    Ok(AuthError::InvalidCredentials)
}

/// `user_id`'s password identity, locked for the rest of the unit of work, once the password of
/// `matched` matched `read`, the hash that the sign-in read without the lock, so two sign-ins of
/// one user that start a session take turns, and one on a device ends the session that the other
/// just started there (AUTH-008, DATA-20). The identity comes back with the password and the turn
/// at hashing of `matched`, or none, which answers as a wrong password does, when the identity is
/// gone by then, or holds a hash that another unit of work wrote meanwhile, which the password,
/// verified again in the turn, doesn't match (AUTH-005).
async fn locked(
    uow: &mut UnitOfWork,
    user_id: UserId,
    read: &str,
    matched: (Password, Turn),
) -> Result<Option<(StoredPassword, Password, Turn)>, AuthError> {
    let (password, hashing) = matched;
    let Some(identity) = repo::password_identity_for_update(uow, user_id).await? else {
        return Ok(None);
    };
    if identity.hash == read {
        return Ok(Some((identity, password, hashing)));
    }
    let ((matches, password), hashing) =
        verify_in(hashing, identity.hash.clone(), password).await?;
    Ok(matches.then_some((identity, password, hashing)))
}
