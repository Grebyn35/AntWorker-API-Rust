//! A user's identities, the ways their account signs in: reading them, adding a password, linking
//! a provider, and removing one (AUTH-085 to AUTH-091, the auth spec's "Identities"). Each change
//! counts in the user's bucket first, takes the user's turn, and needs their session to have
//! proved who they are within the last 5 minutes.

use platform::{
    authn::Principal,
    db::UnitOfWork,
    errors::Internal,
    events::UserEvent,
    rate_limit::RateLimits,
    types::{Client, EmailAddress, UserId},
};

use super::{
    Auth, buckets, hash,
    providers::{Grant, through},
    reauthentication::{require_recent_proof, take_turn},
    sessions::end_sessions,
};
use crate::modules::{
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        emails::{IdentityChange, IdentityChangedEmail},
        error::AuthError,
        events::{IDENTITY_ADDED, IDENTITY_REMOVED, IdentityChanged},
        model::{Identities, Password},
        repo,
    },
};

/// The identity that a removal's ID names: the way that the sessions it started signed in, the
/// provider's key, or `None` for the password (AUTH-088).
type Way = Option<String>;

// part: workspace_setup
/// Whether the account of `user_id` has an identity, a way to sign in, which the operator's setup
/// of a workspace asks of the account that holds its owner's address, once it holds the user's
/// row, so that no change of their identities commits between its answer and the setup's commit
/// (WORKSPACES-024, DATA-20).
pub(crate) async fn has_identity(uow: &mut UnitOfWork, user_id: UserId) -> Result<bool, AuthError> {
    Ok(repo::has_identity(uow, user_id).await?)
}
// end part: workspace_setup

impl Auth {
    /// The caller's identities (AUTH-085).
    pub(crate) async fn identities(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<Identities, AuthError> {
        let mut uow = self.database.begin(client).await?;
        Ok(read(&mut uow, principal.user_id()).await?)
    }

    /// Gives the caller's account, which has no password, `password`, once their session proved
    /// who they are within the last 5 minutes, and answers their identities (AUTH-086).
    pub(crate) async fn add_password(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        password: Password,
    ) -> Result<Identities, AuthError> {
        let user_id = principal.user_id();
        self.count_identity_change(limits, user_id).await?;
        // Hashed before the unit of work opens, so no transaction waits for a turn at hashing.
        let password_hash = hash(&self.workers, password).await?;
        let mut uow = self.database.begin(client).await?;
        let address = take_turn(&mut uow, principal).await?;
        require_recent_proof(&mut uow, principal).await?;
        let id = repo::add_password(&mut uow, user_id, &password_hash)
            .await?
            .ok_or(AuthError::PasswordAlreadySet)?;
        let entry = Entry::new(audit::PASSWORD_ADDED, audit::IDENTITY, id);
        audit_log::record(&mut uow, principal.audit_scope(), entry).await?;
        publish(&mut uow, &IDENTITY_ADDED, user_id, id).await?;
        self.tell_identities_changed(
            &mut uow,
            principal,
            address.email,
            IdentityChange::PasswordAdded,
        )
        .await?;
        let identities = read(&mut uow, user_id).await?;
        uow.commit().await?;
        Ok(identities)
    }

    /// Links the account at `provider` that `grant`'s code names to the caller's account, once
    /// their session proved who they are within the last 5 minutes, and answers their identities
    /// (AUTH-087). The address that the provider gives decides nothing.
    pub(crate) async fn link_provider(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        (provider, grant): (&str, Grant),
    ) -> Result<Identities, AuthError> {
        let user_id = principal.user_id();
        self.count_identity_change(limits, user_id).await?;
        let (provider, identity) = self.identify_signed_in(provider, &grant).await?;
        let mut uow = self.database.begin(client).await?;
        let address = take_turn(&mut uow, principal).await?;
        require_recent_proof(&mut uow, principal).await?;
        // The user's turn keeps their own identities as this reads them; the unique index of
        // subjects decides whether another account holds this one.
        if repo::provider_subject(&mut uow, user_id, provider.key)
            .await?
            .is_some()
        {
            return Err(AuthError::ProviderAlreadyLinked);
        }
        let id = repo::insert_provider_identity(&mut uow, user_id, provider.key, &identity.subject)
            .await?
            .ok_or(AuthError::IdentityInUse)?;
        let entry = through(
            Entry::new(audit::PROVIDER_LINKED, audit::IDENTITY, id),
            provider.key,
        );
        audit_log::record(&mut uow, principal.audit_scope(), entry).await?;
        publish(&mut uow, &IDENTITY_ADDED, user_id, id).await?;
        let change = IdentityChange::ProviderLinked(self.providers.name(provider.key));
        self.tell_identities_changed(&mut uow, principal, address.email, change)
            .await?;
        let identities = read(&mut uow, user_id).await?;
        uow.commit().await?;
        Ok(identities)
    }

    /// Removes the caller's identity `id`, which `None` gives for a path ID that isn't a UUID,
    /// unless it is their last, once their session proved who they are within the last 5
    /// minutes, and ends their other sessions that signed in with it (AUTH-088, PLATFORM-072).
    pub(crate) async fn remove_identity(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        id: Option<uuid::Uuid>,
    ) -> Result<(), AuthError> {
        let user_id = principal.user_id();
        self.count_identity_change(limits, user_id).await?;
        let mut uow = self.database.begin(client).await?;
        let address = take_turn(&mut uow, principal).await?;
        require_recent_proof(&mut uow, principal).await?;
        // The user's turn keeps their identities as this reads them, so of two removals at once,
        // the second finds what the first left (DATA-20).
        let identities = read(&mut uow, user_id).await?;
        let (id, way) = id
            .and_then(|id| named(&identities, id).map(|way| (id, way)))
            .ok_or(AuthError::IdentityNotFound)?;
        if usize::from(identities.password.is_some()) + identities.providers.len() == 1 {
            return Err(AuthError::LastIdentity);
        }
        let (entry, change) = match &way {
            // The identity before any challenge or session, as a sign-in locks them (the auth
            // spec's "Taking turns").
            None => {
                repo::delete_password_identity(&mut uow, user_id, id).await?;
                (
                    Entry::new(audit::PASSWORD_REMOVED, audit::IDENTITY, id),
                    IdentityChange::PasswordRemoved,
                )
            }
            Some(provider) => {
                repo::delete_provider_identity(&mut uow, user_id, id).await?;
                (
                    through(
                        Entry::new(audit::PROVIDER_UNLINKED, audit::IDENTITY, id),
                        provider,
                    ),
                    IdentityChange::ProviderUnlinked(self.providers.name(provider)),
                )
            }
        };
        // No challenge that a sign-in with it issued completes (AUTH-069).
        repo::second_factors::delete_challenges_through(&mut uow, user_id, way.as_deref()).await?;
        let ended = repo::delete_other_sessions_through(
            &mut uow,
            user_id,
            principal.session_id(),
            way.as_deref(),
        )
        .await?;
        end_sessions(&mut uow, principal.audit_scope(), user_id, &ended).await?;
        audit_log::record(&mut uow, principal.audit_scope(), entry).await?;
        publish(&mut uow, &IDENTITY_REMOVED, user_id, id).await?;
        self.tell_identities_changed(&mut uow, principal, address.email, change)
            .await?;
        uow.commit().await?;
        Ok(())
    }

    /// Counts a change of `user_id`'s identities in their bucket, since each sends their address
    /// an email (AUTH-090, SEC-13).
    async fn count_identity_change(
        &self,
        limits: &RateLimits,
        user_id: UserId,
    ) -> Result<(), AuthError> {
        Ok(limits
            .check(buckets::IDENTITY_CHANGE, user_id.uuid().as_bytes())
            .await?)
    }

    /// Enqueues in `uow` the email that tells `to`, the caller's address, that their account's
    /// identities changed, while email is on (AUTH-091).
    async fn tell_identities_changed(
        &self,
        uow: &mut UnitOfWork,
        principal: &Principal,
        to: EmailAddress,
        change: IdentityChange,
    ) -> Result<(), Internal> {
        if !self.mail.is_on() {
            return Ok(());
        }
        let email = IdentityChangedEmail { to, change }.render()?;
        self.mail.send(uow, principal.job_scope(), email).await
    }
}

/// `user_id`'s identities, as `uow` reads them (AUTH-085).
async fn read(uow: &mut UnitOfWork, user_id: UserId) -> Result<Identities, Internal> {
    Ok(Identities {
        password: repo::password_identity(uow, user_id).await?,
        providers: repo::provider_identities(uow, user_id).await?,
    })
}

/// The way that the identity `id` of `identities` signs in, or `None` when the ID names none of
/// them.
fn named(identities: &Identities, id: uuid::Uuid) -> Option<Way> {
    if identities
        .password
        .as_ref()
        .is_some_and(|password| password.id == id)
    {
        return Some(None);
    }
    identities
        .providers
        .iter()
        .find(|identity| identity.id == id)
        .map(|identity| Some(identity.provider.clone()))
}

/// Publishes `event`, that the identity `id` joined or left `user_id`'s identities, to their
/// streams (AUTH-089).
async fn publish(
    uow: &mut UnitOfWork,
    event: &UserEvent<IdentityChanged>,
    user_id: UserId,
    id: uuid::Uuid,
) -> Result<(), Internal> {
    uow.publish_to_user(event, user_id, &IdentityChanged { identity_id: id })
        .await
}
