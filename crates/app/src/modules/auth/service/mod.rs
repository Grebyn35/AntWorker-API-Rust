//! Registration, sign-in, refresh and sign-out; in `reauthentication.rs`, re-authenticating; in
//! `deletion.rs`, confirming an account's deletion; and in `verifier.rs`, the verifier of access
//! tokens and sessions. Each operation opens its own unit of work, after any per-action limit it
//! counts first.

use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use platform::{
    authn::{AuditScope, Principal},
    blocking::Workers,
    db::{Database, UnitOfWork},
    errors::Internal,
    events::{self, AccessChange},
    hashing::{HmacKey, sha256},
    random,
    rate_limit::{self, Bucket, LimitError, RateLimits},
    types::{Client, EmailAddress, Secret, SessionId, UserId},
};

mod deletion;
mod devices;
mod reauthentication;
mod verifier;

pub(super) use deletion::erase;
pub(crate) use verifier::Verifier;

use super::{
    audit,
    error::AuthError,
    metrics,
    model::{self, DeviceId, DeviceToken, Lifetimes, Password, Session, Tokens},
    repo,
};
use crate::modules::{
    accounts::{self, Creation, Deletion, DisplayName},
    audit::{self as audit_log, Entry},
};

/// A registration's input, in domain types (AUTH-001).
pub(crate) struct Registration {
    pub(crate) email: EmailAddress,
    pub(crate) password: Password,
    pub(crate) name: DisplayName,
    pub(crate) device_id: Option<DeviceId>,
}

/// A sign-in's input, in domain types (AUTH-004).
pub(crate) struct SignIn {
    pub(crate) email: EmailAddress,
    pub(crate) password: Password,
    pub(crate) device_id: Option<DeviceId>,
    /// The device token the client holds, if any, which counts as none unless it is valid for the
    /// email (AUTH-019).
    pub(crate) device_token: Option<Secret<String>>,
    /// A browser's device tokens, by the names of their cookies after `__Host-`, of which the
    /// sign-in reads its email's when `device_token` is none (AUTH-015).
    pub(crate) device_cookies: Vec<(String, Secret<String>)>,
}

/// What `auth` takes from the deployment.
pub(crate) struct Settings {
    /// How long a session's tokens last (AUTH-007).
    pub(crate) lifetimes: Lifetimes,
    /// The sign-in limit of an email from one client address (AUTH-006).
    pub(crate) sign_in: Bucket,
    /// The sign-in limit of an email from every address, which counts only the attempts that
    /// `sign_in` lets through (AUTH-006).
    pub(crate) sign_in_email: Bucket,
    /// The sign-in limit of a device token valid for the email, in place of the other two
    /// (AUTH-006).
    pub(crate) sign_in_device: Bucket,
    /// How long a device token lasts (AUTH-019).
    pub(crate) device_tokens: std::time::Duration,
    /// The bucket of each user's checks of their password, once signed in (AUTH-022).
    pub(crate) password_check: Bucket,
    /// The bucket of the deletions of an email's accounts, each of which sends the address an
    /// email (AUTH-024).
    pub(crate) deletion_email: Bucket,
}

/// Registration, sign-in and sessions.
pub(crate) struct Auth {
    database: Database,
    settings: Settings,
    /// The key of the keyed hash of a device token's email (AUTH-019).
    key: HmacKey,
    /// A hash of a random password, verified when a sign-in finds no password to check, so every
    /// failed sign-in costs the same (AUTH-005).
    dummy_hash: String,
    /// What hashes and verifies passwords (AUTH-014).
    workers: Workers,
    /// What deletes an account across every module, once this module confirms it (AUTH-023).
    deletion: Deletion,
}

impl Auth {
    /// The service, with its dummy hash computed and the breached-password list loaded now, at
    /// startup, rather than in the first request that needs them (CODE-05).
    pub(crate) async fn new(
        database: Database,
        settings: Settings,
        key: HmacKey,
        workers: Workers,
        deletion: Deletion,
    ) -> Result<Self, Internal> {
        let dummy = Password::generated(random::token()?);
        let dummy_hash = hash(&workers, dummy).await?;
        workers
            .run("load_breached_passwords", model::load_breached_passwords)
            .await
            .map_err(Internal::new)?;
        Ok(Self {
            database,
            settings,
            key,
            dummy_hash,
            workers,
            deletion,
        })
    }

    /// Registers a user with a password identity and a first session, in one unit of work, and
    /// issues their device a device token (AUTH-001, AUTH-002, AUTH-019).
    pub(crate) async fn register(
        &self,
        client: &Client,
        input: Registration,
    ) -> Result<(Session, Tokens, DeviceToken), AuthError> {
        let password_hash = hash(&self.workers, input.password).await?;
        let mut uow = self.database.begin(client).await?;
        let user_id = match accounts::create(&mut uow, &input.email, &input.name).await? {
            Creation::Created(id) => id,
            Creation::EmailInUse => return Err(AuthError::EmailInUse),
        };
        repo::insert_password_identity(&mut uow, user_id, &password_hash).await?;
        let (principal, session, tokens) = self
            .start_session(&mut uow, user_id, input.device_id.as_ref())
            .await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::USER_REGISTERED, audit::USER, user_id.uuid()),
        )
        .await?;
        let device = self
            .issue_device_token(&mut uow, user_id, &input.email, None)
            .await?;
        uow.commit().await?;
        Ok((session, tokens, device))
    }

    /// Signs in with an email and a password (AUTH-004 to AUTH-008), issuing the device a device
    /// token (AUTH-019), and counts how the attempt ended in `auth.sign_ins`.
    pub(crate) async fn sign_in(
        &self,
        limits: &RateLimits,
        client: &Client,
        input: SignIn,
    ) -> Result<(Session, Tokens, DeviceToken), AuthError> {
        let result = self.attempt(limits, client, input).await;
        match &result {
            Ok(_) => metrics::sign_in(metrics::SignIn::Succeeded),
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
    ) -> Result<(Session, Tokens, DeviceToken), AuthError> {
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
        let (principal, session, tokens) = self
            .replace_session(&mut uow, user_id, input.device_id.as_ref())
            .await?;
        if !identity.hash.starts_with(&self.current_parameters()) {
            let rehashed = hash(&self.workers, password).await?;
            repo::update_password_hash(&mut uow, user_id, identity.id, &rehashed).await?;
            audit_log::record(
                &mut uow,
                principal.audit_scope(),
                Entry::new(audit::PASSWORD_REHASHED, audit::IDENTITY, identity.id),
            )
            .await?;
        }
        let device = self
            .issue_device_token(&mut uow, user_id, &input.email, trusted.as_ref())
            .await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::DEVICE_TOKEN_ISSUED, audit::DEVICE_TOKEN, device.id),
        )
        .await?;
        uow.commit().await?;
        Ok((session, tokens, device))
    }

    /// Starts a session of `user_id`, ending the one its device has, if any, through the full
    /// sign-out path (AUTH-008).
    async fn replace_session(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        device_id: Option<&DeviceId>,
    ) -> Result<(Principal, Session, Tokens), AuthError> {
        let replaced = match device_id {
            Some(device_id) => repo::session_on_device(uow, user_id, device_id).await?,
            None => None,
        };
        if let Some(old) = replaced {
            end_session(uow, user_id, old).await?;
        }
        let (principal, session, tokens) = self.start_session(uow, user_id, device_id).await?;
        if let Some(old) = replaced {
            audit_log::record(
                uow,
                principal.audit_scope(),
                Entry::new(audit::SESSION_ENDED, audit::SESSION, old.uuid()),
            )
            .await?;
        }
        audit_log::record(
            uow,
            principal.audit_scope(),
            Entry::new(audit::SESSION_CREATED, audit::SESSION, session.id.uuid()),
        )
        .await?;
        Ok((principal, session, tokens))
    }

    /// Gives the session of the refresh token `token` a new pair of tokens (AUTH-010 to
    /// AUTH-012).
    pub(crate) async fn refresh(
        &self,
        client: &Client,
        token: &str,
    ) -> Result<(Session, Tokens), AuthError> {
        let presented = sha256(token.as_bytes());
        let mut uow = self.database.begin(client).await?;
        let Some(found) = repo::by_refresh_token(&mut uow, &presented).await? else {
            if let Some(reused) = repo::by_previous_refresh_token(&mut uow, &presented).await? {
                end_session(&mut uow, reused.user_id, reused.id).await?;
                audit_log::record(
                    &mut uow,
                    AuditScope::attempt(reused.user_id),
                    Entry::new(
                        audit::SESSION_REUSE_DETECTED,
                        audit::SESSION,
                        reused.id.uuid(),
                    ),
                )
                .await?;
                uow.commit().await?;
            }
            return Err(AuthError::InvalidRefreshToken);
        };
        if found.expired {
            return Err(AuthError::SessionExpired);
        }
        let tokens = new_tokens()?;
        let session = repo::rotate(
            &mut uow,
            &found,
            &presented,
            &sha256(tokens.access.expose().as_bytes()),
            &sha256(tokens.refresh.expose().as_bytes()),
            self.settings.lifetimes,
        )
        .await?
        .ok_or(AuthError::InvalidRefreshToken)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "auth builds the principal of a session the refresh token proved (the auth spec)"
        )]
        let principal = Principal::new(found.user_id, found.id);
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::SESSION_REFRESHED, audit::SESSION, found.id.uuid()),
        )
        .await?;
        uow.commit().await?;
        Ok((session, tokens))
    }

    /// The session that authenticated the caller (AUTH-016).
    pub(crate) async fn current(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<Session, AuthError> {
        let mut uow = self.database.begin(client).await?;
        repo::session(&mut uow, principal.user_id(), principal.session_id())
            .await?
            .ok_or_else(|| Internal::new("the caller's session doesn't exist").into())
    }

    /// Ends the session that authenticated the caller (AUTH-013).
    pub(crate) async fn sign_out(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<(), AuthError> {
        let mut uow = self.database.begin(client).await?;
        let (user_id, session_id) = (principal.user_id(), principal.session_id());
        end_session(&mut uow, user_id, session_id).await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::SESSION_ENDED, audit::SESSION, session_id.uuid()),
        )
        .await?;
        uow.commit().await?;
        Ok(())
    }

    /// Creates a session of `user_id`, with new tokens, and the principal it proves.
    async fn start_session(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        device_id: Option<&DeviceId>,
    ) -> Result<(Principal, Session, Tokens), AuthError> {
        let tokens = new_tokens()?;
        let session = repo::insert_session(
            uow,
            user_id,
            device_id,
            &sha256(tokens.access.expose().as_bytes()),
            &sha256(tokens.refresh.expose().as_bytes()),
            self.settings.lifetimes,
        )
        .await?;
        #[expect(
            clippy::disallowed_methods,
            reason = "auth builds the principal of a session it just created (the auth spec)"
        )]
        let principal = Principal::new(user_id, session.id);
        Ok((principal, session, tokens))
    }

    /// The start of a hash made with the current parameters, such as
    /// `$argon2id$v=19$m=19456,t=2,p=1$`: a stored hash that doesn't start with it was made with
    /// others, and is rehashed (AUTH-014).
    fn current_parameters(&self) -> String {
        let parts: Vec<&str> = self.dummy_hash.splitn(5, '$').take(4).collect();
        format!("{}$", parts.join("$"))
    }
}

/// Deletes a session, and signals that it ended, so its streams close (AUTH-013, D35).
async fn end_session(
    uow: &mut UnitOfWork,
    user_id: UserId,
    session_id: SessionId,
) -> Result<(), Internal> {
    repo::delete_session(uow, user_id, session_id).await?;
    events::signal(
        uow,
        AccessChange::SessionEnded {
            user_id,
            session_id,
        },
    )
    .await
}

fn new_tokens() -> Result<Tokens, Internal> {
    Ok(Tokens {
        access: Secret::new(random::token()?),
        refresh: Secret::new(random::token()?),
    })
}

/// The Argon2id hash of `password`, in PHC format, with a new salt, through `workers`
/// (AUTH-014).
async fn hash(workers: &Workers, password: Password) -> Result<String, Internal> {
    let salt = random::bytes::<16>()?;
    workers
        .run("hash_password", move || {
            Argon2::default()
                .hash_password_with_salt(password.bytes(), &salt)
                .map(|hash| hash.to_string())
                .map_err(|error| Internal::new(error.to_string()))
        })
        .await
        .map_err(Internal::new)?
}

/// Whether `password` matches `stored`, through `workers`, with the password handed back for a
/// rehash (AUTH-005, AUTH-014).
async fn verify(
    workers: &Workers,
    stored: String,
    password: Password,
) -> Result<(bool, Password), Internal> {
    workers
        .run("verify_password", move || {
            let matches = Argon2::default()
                .verify_password(password.bytes(), stored.as_str())
                .is_ok();
            (matches, password)
        })
        .await
        .map_err(Internal::new)
}
