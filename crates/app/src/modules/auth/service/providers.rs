//! Signing in, registering and re-authenticating through an OAuth provider (AUTH-073 to AUTH-080,
//! the auth spec's "Signing in through a provider").

use platform::{
    authn::{AuditScope, Principal},
    db::UnitOfWork,
    errors::{FieldError, Internal, RequestError},
    rate_limit::RateLimits,
    types::{Client, EmailAddress, Secret, UserId},
};

use super::{Auth, Device, SignedIn, buckets, reauthentication::take_turn};
use crate::modules::{
    accounts::{self, Address, Creation, DisplayName},
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        error::AuthError,
        metrics,
        model::{FactorProof, Required, Session},
        providers::{self, Exchange, Found, Identity, Provider, ProviderError},
        repo,
    },
};

/// What a request exchanges at a provider: the code that the provider's authorization gave, the
/// verifier of its PKCE challenge, and the redirect URI that received the code (AUTH-073).
pub(crate) struct Grant {
    pub(crate) code: Secret<String>,
    pub(crate) verifier: Secret<String>,
    pub(crate) redirect_uri: String,
}

/// A sign-in through a provider: the provider that its `provider` names, its grant, the device,
/// and the device token the client holds, if any (AUTH-073, AUTH-019).
pub(crate) struct ProviderSignIn {
    pub(crate) provider: String,
    pub(crate) grant: Grant,
    pub(crate) device: Device,
    pub(crate) device_token: Option<Secret<String>>,
    /// A browser's device tokens, by the names of their cookies after `__Host-` (AUTH-015).
    pub(crate) device_cookies: Vec<(String, Secret<String>)>,
}

/// The longest a name of a new account is, cut from its address (AUTH-074).
const NAME_MAX_CHARS: usize = 100;

impl Auth {
    /// Signs in through a provider, registering when no account is linked to the provider's
    /// subject and none holds its address (AUTH-073 to AUTH-076).
    pub(crate) async fn sign_in_through(
        &self,
        client: &Client,
        input: ProviderSignIn,
    ) -> Result<SignedIn, AuthError> {
        let (provider, identity) = self.identify(&input.provider, &input.grant).await?;
        let mut uow = self.database.begin(client).await?;
        if let Some(user_id) =
            repo::user_of_subject(&mut uow, provider.id(), &identity.subject).await?
            // The user's turn, as a deletion of the account takes it first: a sign-in that waited
            // for one finds no user, and registers (the auth spec's "Taking turns").
            && let Some(address) = accounts::address_for_update(&mut uow, user_id).await?
        {
            return self
                .sign_in_linked(uow, (user_id, address), &identity, input)
                .await;
        }
        self.register_through(uow, provider, identity, input).await
    }

    /// Signs in to `user`'s account, whose provider identity holds the subject, as a sign-in with
    /// a password does, proving its address when the provider is the address's authority
    /// (AUTH-073, AUTH-075).
    async fn sign_in_linked(
        &self,
        mut uow: UnitOfWork,
        user: (UserId, Address),
        identity: &Identity,
        input: ProviderSignIn,
    ) -> Result<SignedIn, AuthError> {
        let (user_id, address) = user;
        // The password identity, as a sign-in with the password locks it, so the user's sign-ins
        // take turns whatever their ways in (AUTH-008, DATA-20).
        repo::password_identity_for_update(&mut uow, user_id).await?;
        if identity.proved
            && !address.verified
            && proved_address(identity).as_ref() == Some(&address.email)
        {
            accounts::verify_email(&mut uow, user_id).await?;
            audit_log::record(
                &mut uow,
                AuditScope::attempt(user_id),
                Entry::new(audit::EMAIL_VERIFIED, audit::USER, user_id.uuid()),
            )
            .await?;
        }
        let trusted = self
            .trusted_in(
                &mut uow,
                &address.email,
                &input.device_token,
                &input.device_cookies,
            )
            .await?;
        if repo::second_factors::has_app(&mut uow, user_id).await? {
            let challenge = self
                .challenge(&mut uow, user_id, &input.device, trusted.as_ref())
                .await?;
            uow.commit().await?;
            return Ok(SignedIn::Challenged(challenge));
        }
        let (principal, session, tokens) = self
            .replace_session(&mut uow, user_id, &input.device)
            .await?;
        let device = self
            .issue_device_token(&mut uow, user_id, &address.email, trusted.as_ref())
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

    /// Registers a user through `provider`, with the address and the name that it gives, a
    /// provider identity that holds its subject, a first session and a device token, unless an
    /// account holds the address (AUTH-074 to AUTH-076).
    async fn register_through(
        &self,
        mut uow: UnitOfWork,
        provider: &dyn Provider,
        identity: Identity,
        input: ProviderSignIn,
    ) -> Result<SignedIn, AuthError> {
        let email = identity
            .email
            .as_deref()
            .and_then(|email| EmailAddress::parse(email).ok())
            .ok_or(AuthError::ProviderAddressMissing)?;
        let name = name(identity.name.as_deref(), &email)?;
        let user_id = match accounts::create(&mut uow, &email, &name).await? {
            Creation::Created(id) => id,
            Creation::EmailInUse => return Err(AuthError::EmailInUse),
        };
        repo::insert_provider_identity(&mut uow, user_id, provider.id(), &identity.subject).await?;
        if identity.proved {
            accounts::verify_email(&mut uow, user_id).await?;
        }
        let (principal, session, tokens) =
            self.start_session(&mut uow, user_id, &input.device).await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            through(
                Entry::new(audit::USER_REGISTERED, audit::USER, user_id.uuid()),
                provider.id(),
            ),
        )
        .await?;
        let device = self
            .issue_device_token(&mut uow, user_id, &email, None)
            .await?;
        if !identity.proved {
            self.enqueue_code(&mut uow, &principal).await?;
        }
        uow.commit().await?;
        let required = Required {
            email_verification: self.mail.is_on() && !identity.proved,
            second_factor_setup: self.settings.second_factor_required,
        };
        Ok(SignedIn::Session(Box::new((
            session.answer(required),
            tokens,
            device,
        ))))
    }

    /// Re-authenticates the session that authenticated the caller through `provider`, when the
    /// provider's subject is the caller's, with the second factor's code of an account that has
    /// one, and answers it, or records a subject that isn't theirs (AUTH-080).
    pub(crate) async fn reauthenticate_through(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        (provider, grant): (&str, Grant),
        proof: Option<FactorProof>,
    ) -> Result<Session, AuthError> {
        let user_id = principal.user_id();
        self.count_signed_in_check(limits, principal, proof.as_ref())
            .await?;
        limits
            .check(buckets::PASSWORD_CHECK, user_id.uuid().as_bytes())
            .await?;
        let (provider, identity) = self.identify(provider, &grant).await?;
        let mut uow = self.database.begin(client).await?;
        take_turn(&mut uow, principal).await?;
        let subject = repo::provider_subject(&mut uow, user_id, provider.id()).await?;
        if subject.as_deref() != Some(identity.subject.as_str()) {
            audit_log::record(
                &mut uow,
                principal.audit_scope(),
                through(
                    Entry::new(audit::IDENTITY_CHECK_FAILED, audit::USER, user_id.uuid()),
                    provider.id(),
                ),
            )
            .await?;
            uow.commit().await?;
            return Err(AuthError::WrongIdentity);
        }
        let mut uow = self
            .check_signed_in_factor(uow, principal, proof.as_ref())
            .await?;
        // Another request may have ended the session since it authenticated this one.
        let session = repo::reauthenticate(&mut uow, user_id, principal.session_id())
            .await?
            .ok_or(RequestError::Unauthenticated)?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            through(
                Entry::new(
                    audit::SESSION_REAUTHENTICATED,
                    audit::SESSION,
                    session.id.uuid(),
                ),
                provider.id(),
            ),
        )
        .await?;
        let session = self.answer(&mut uow, session).await?;
        uow.commit().await?;
        Ok(session)
    }

    /// The provider that `id` names, and the identity that `grant`'s code gives there, which its
    /// exchange reads before any unit of work opens, and counts in `auth.provider_exchanges`
    /// (AUTH-077 to AUTH-079).
    async fn identify(
        &self,
        id: &str,
        grant: &Grant,
    ) -> Result<(&dyn Provider, Identity), AuthError> {
        let provider = match self.providers.find(id) {
            Found::On(provider) => provider,
            Found::Off => return Err(RequestError::FeatureDisabled.into()),
            Found::Unknown => return Err(AuthError::ProviderNotFound),
        };
        let client = providers::client_for(provider, &grant.redirect_uri).ok_or_else(|| {
            RequestError::InvalidFields(vec![FieldError::new(
                "/redirect_uri",
                "invalid_redirect_uri",
                "The redirect URI is none that the provider's clients list.",
            )])
        })?;
        let exchange = Exchange {
            client,
            code: &grant.code,
            verifier: &grant.verifier,
            redirect_uri: &grant.redirect_uri,
        };
        let identified = providers::identify(provider, &self.outbound, exchange).await;
        let outcome = match &identified {
            Ok(_) => metrics::Exchange::Succeeded,
            Err(ProviderError::Rejected) => metrics::Exchange::Rejected,
            Err(ProviderError::Unavailable(_)) => metrics::Exchange::Unavailable,
        };
        metrics::exchange(provider.id(), outcome);
        match identified {
            Ok(identity) => Ok((provider, identity)),
            Err(ProviderError::Rejected) => Err(AuthError::ProviderCodeRejected),
            Err(ProviderError::Unavailable(cause)) => Err(AuthError::ProviderUnavailable(
                Internal::new(format!("{}: {cause}", provider.id())),
            )),
        }
    }
}

/// `entry`, with the provider whose sign-in it records in its details (AUTH-073).
pub(super) fn through(mut entry: Entry, provider: &str) -> Entry {
    entry.details = serde_json::json!({ "provider": provider });
    entry
}

/// The address that `identity`'s provider proves, as ACCOUNTS-002 normalizes it (AUTH-075).
fn proved_address(identity: &Identity) -> Option<EmailAddress> {
    identity
        .email
        .as_deref()
        .and_then(|email| EmailAddress::parse(email).ok())
}

/// The name of an account that registers through a provider: the provider's, which ACCOUNTS-003
/// normalizes, or, without one that it allows, the address's part before its `@`, cut to 100
/// characters (AUTH-074).
fn name(given: Option<&str>, email: &EmailAddress) -> Result<DisplayName, Internal> {
    if let Some(name) = given.and_then(|name| DisplayName::parse(name).ok()) {
        return Ok(name);
    }
    let local: String = email
        .as_str()
        .split('@')
        .next()
        .unwrap_or_default()
        .chars()
        .take(NAME_MAX_CHARS)
        .collect();
    DisplayName::parse(&local)
        .map_err(|_| Internal::new("an address's part before its @ is no display name"))
}
