//! Registration and refresh; in `sign_in.rs`, signing in; in `sessions.rs`, reading, listing and
//! ending a user's sessions, signing out among them; in `reauthentication.rs`, re-authenticating;
//! in `deletion.rs`, confirming an account's deletion; in `exports.rs`, confirming the request of
//! an export of the caller's data; in `verification.rs`, proving an account's
//! email address, and the job that sends every code; in `password.rs`, resetting and changing the
//! password; in `email_change.rs`, changing the account's address; in `second_factors/`, second
//! factors and the completion of a sign-in with one; and in `verifier.rs`, the verifier of access
//! tokens and sessions. Each operation opens its own unit of work, after any per-action limit it
//! counts first.

use std::sync::Arc;

use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use platform::{
    authn::{AuditScope, Principal},
    blocking::Workers,
    db::{Database, UnitOfWork},
    encryption::Keyring,
    errors::{Internal, RequestError},
    hashing::{HmacKey, sha256},
    mail::Mail,
    outbound::Configured,
    random,
    rate_limit::{Bucket, RateLimits},
    types::{Client, EmailAddress, Secret, UserId},
};

mod buckets;
mod deletion;
mod devices;
mod email_change;
mod exports;
mod password;
mod providers;
mod reauthentication;
mod second_factors;
mod sessions;
mod sign_in;
mod verification;
mod verifier;

pub(super) use deletion::erase;
pub(crate) use email_change::ChangeConfirmation;
pub(crate) use password::{PasswordChange, ResetConfirmation};
pub(crate) use providers::{Grant, ProviderSignIn};
pub(crate) use second_factors::{
    Challenge, Completion, Removal, SecondFactors, Setup, remove_second_factor,
};
pub(crate) use sessions::Ended;
pub(crate) use sign_in::{SignIn, SignedIn};
pub(crate) use verifier::Verifier;

use super::{
    audit,
    error::AuthError,
    events::{SESSION_CREATED, SessionChanged},
    model::{
        self, DeviceId, DeviceName, DeviceToken, Lifetimes, Password, Required, Session,
        StoredSession, Tokens,
    },
    providers::Providers,
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
    pub(crate) device: Device,
}

/// The device a registration or a sign-in comes from: its ID and its name, either of which it may
/// leave out (AUTH-008, AUTH-042).
pub(crate) struct Device {
    pub(crate) id: Option<DeviceId>,
    pub(crate) name: Option<DeviceName>,
    /// The provider whose sign-in the device makes, or `None` for a password, which the session
    /// records (AUTH-016, AUTH-073).
    pub(crate) provider: Option<String>,
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
    /// Whether every account must have a second factor, `SECOND_FACTOR_REQUIRED` (AUTH-070,
    /// D133).
    pub(crate) second_factor_required: bool,
}

/// The keys that `auth` takes from the deployment: the key of its keyed hashes, and the ring that
/// encrypts each authenticator app's secret (SEC-03, SEC-02).
pub(crate) struct Keys {
    pub(crate) hmac: HmacKey,
    pub(crate) encryption: Keyring,
}

impl Keys {
    /// The keys of `server`'s settings, `HMAC_KEY` and `ENCRYPTION_KEYS`.
    pub(crate) fn of(server: &platform::config::ServerConfig) -> Self {
        Self {
            hmac: server.hmac_key.clone(),
            encryption: server.encryption_keys.clone(),
        }
    }
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
    /// What sends the codes that prove an account's address or reset its password, and the email
    /// that tells it its password changed, and says whether email is on (AUTH-025, AUTH-030,
    /// AUTH-036, AUTH-041).
    mail: Mail,
    /// The keys that encrypt each authenticator app's secret (AUTH-066).
    keyring: Keyring,
    /// The OAuth providers that the deployment configures (AUTH-081).
    providers: Arc<Providers>,
    /// The client of the providers' endpoints (SEC-10).
    outbound: Configured,
}

/// What `auth` reaches beyond the database: the deployment's email, and its OAuth providers,
/// through the configured client (AUTH-025, AUTH-073).
pub(crate) struct Outside {
    /// What sends the codes and the emails, and says whether email is on.
    pub(crate) mail: Mail,
    /// The OAuth providers that the deployment configures.
    pub(crate) providers: Arc<Providers>,
    /// The client of the providers' endpoints.
    pub(crate) outbound: Configured,
}

impl Auth {
    /// The service, with its dummy hash computed and the breached-password list loaded now, at
    /// startup, rather than in the first request that needs them (CODE-05), once it has refused a
    /// key ring that lacks a version that a stored secret needs (AUTH-066).
    pub(crate) async fn new(
        database: Database,
        settings: Settings,
        keys: Keys,
        workers: Workers,
        deletion: Deletion,
        outside: Outside,
    ) -> Result<Self, Internal> {
        let Outside {
            mail,
            providers,
            outbound,
        } = outside;
        let Keys {
            hmac: key,
            encryption: keyring,
        } = keys;
        let dummy = Password::generated(random::token()?);
        let dummy_hash = hash(&workers, dummy).await?;
        workers
            .run("load_breached_passwords", model::load_breached_passwords)
            .await
            .map_err(Internal::new)?;
        second_factors::refuse_missing_versions(&database, &keyring).await?;
        Ok(Self {
            database,
            settings,
            key,
            dummy_hash,
            workers,
            deletion,
            mail,
            keyring,
            providers,
            outbound,
        })
    }

    /// Registers a user with a password identity and a first session, in one unit of work, issues
    /// their device a device token, and, with email on, enqueues the job that sends their first
    /// code, once the registration has counted in the bucket of the address the code goes to
    /// (AUTH-001, AUTH-002, AUTH-019, AUTH-027, AUTH-029).
    pub(crate) async fn register(
        &self,
        limits: &RateLimits,
        client: &Client,
        input: Registration,
    ) -> Result<(Session, Tokens, DeviceToken), AuthError> {
        if self.mail.is_on() {
            limits
                .check(buckets::REGISTRATION_EMAIL, input.email.as_str().as_bytes())
                .await?;
        }
        let password_hash = hash(&self.workers, input.password).await?;
        let mut uow = self.database.begin(client).await?;
        let user_id = match accounts::create(&mut uow, &input.email, &input.name).await? {
            Creation::Created(id) => id,
            Creation::EmailInUse => return Err(AuthError::EmailInUse),
        };
        repo::insert_password_identity(&mut uow, user_id, &password_hash).await?;
        let (principal, session, tokens) =
            self.start_session(&mut uow, user_id, &input.device).await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::USER_REGISTERED, audit::USER, user_id.uuid()),
        )
        .await?;
        let device = self
            .issue_device_token(&mut uow, user_id, &input.email, None)
            .await?;
        self.enqueue_code(&mut uow, &principal).await?;
        uow.commit().await?;
        // A new account has proved no address and has no app.
        let required = Required {
            email_verification: self.mail.is_on(),
            second_factor_setup: self.settings.second_factor_required,
        };
        Ok((session.answer(required), tokens, device))
    }

    /// Starts a session of `user_id` on `device`, ending the one its device has, if any, through
    /// the full sign-out path (AUTH-008).
    async fn replace_session(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        device: &Device,
    ) -> Result<(Principal, StoredSession, Tokens), AuthError> {
        let replaced = match &device.id {
            Some(device_id) => repo::session_on_device(uow, user_id, device_id).await?,
            None => None,
        };
        let mut ended = Vec::new();
        if let Some(old) = replaced
            && repo::delete_session(uow, user_id, old).await?
        {
            ended.push(old);
        }
        let (principal, session, tokens) = self.start_session(uow, user_id, device).await?;
        sessions::end_sessions(uow, principal.audit_scope(), user_id, &ended).await?;
        let entry = Entry::new(audit::SESSION_CREATED, audit::SESSION, session.id.uuid());
        let entry = match &device.provider {
            Some(provider) => providers::through(entry, provider),
            None => entry,
        };
        audit_log::record(uow, principal.audit_scope(), entry).await?;
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
            // A session that another request ended first, as a deletion of its account does, or
            // that the hourly job deleted, leaves the token naming none, which records nothing
            // (the auth spec's "A user's sessions").
            if let Some(reused) = repo::by_previous_refresh_token(&mut uow, &presented).await?
                && repo::delete_session(&mut uow, reused.user_id, reused.id).await?
            {
                sessions::close(&mut uow, reused.user_id, &[reused.id]).await?;
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
        let session = self.answer(&mut uow, session).await?;
        uow.commit().await?;
        Ok((session, tokens))
    }

    /// `session` as its client sees it, which says whether its account must still prove its
    /// email address or set up a second factor, as `uow` reads the account (AUTH-026, AUTH-070).
    /// An account that a deletion took since the session was read went with the session, so the
    /// request answers as one sent after the deletion does (AUTH-016, ACCOUNTS-019).
    async fn answer(
        &self,
        uow: &mut UnitOfWork,
        session: StoredSession,
    ) -> Result<Session, AuthError> {
        let required = self
            .required(uow, session.user_id)
            .await?
            .ok_or(RequestError::Unauthenticated)?;
        Ok(session.answer(required))
    }

    /// What `user_id`'s account must still do before the API serves it anything else, as `uow`
    /// reads it: prove its email address (AUTH-025), or set up a second factor, while the
    /// deployment requires one (AUTH-070); or `None` once the account is gone.
    async fn required(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
    ) -> Result<Option<Required>, Internal> {
        let Some(email_verification) = self.email_verification_required(uow, user_id).await? else {
            return Ok(None);
        };
        let second_factor_setup = self.settings.second_factor_required
            && !repo::second_factors::has_app(uow, user_id).await?;
        Ok(Some(Required {
            email_verification,
            second_factor_setup,
        }))
    }

    /// Creates a session of `user_id` on `device`, with new tokens, and the principal it proves,
    /// and publishes `session.created` to the user's streams (AUTH-046).
    async fn start_session(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        device: &Device,
    ) -> Result<(Principal, StoredSession, Tokens), AuthError> {
        let tokens = new_tokens()?;
        let session = repo::insert_session(
            uow,
            user_id,
            (
                device.id.as_ref(),
                device.name.as_ref(),
                device.provider.as_deref(),
            ),
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
        let payload = SessionChanged {
            session_id: session.id,
        };
        uow.publish_to_user(&SESSION_CREATED, user_id, &payload)
            .await?;
        Ok((principal, session, tokens))
    }

    /// The database, for the jobs that delete the sessions that stopped lasting, and the setups
    /// and challenges that expired (AUTH-047, AUTH-067).
    pub(super) fn database(&self) -> Database {
        self.database.clone()
    }

    /// The key ring, for the job that encrypts apps' secrets again (AUTH-066).
    pub(super) fn keyring(&self) -> Keyring {
        self.keyring.clone()
    }

    /// The start of a hash made with the current parameters, such as
    /// `$argon2id$v=19$m=19456,t=2,p=1$`: a stored hash that doesn't start with it was made with
    /// others, and is rehashed (AUTH-014).
    fn current_parameters(&self) -> String {
        let parts: Vec<&str> = self.dummy_hash.splitn(5, '$').take(4).collect();
        format!("{}$", parts.join("$"))
    }
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
