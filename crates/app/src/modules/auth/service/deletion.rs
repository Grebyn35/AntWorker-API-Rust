//! Confirming the deletion of the caller's account, which `accounts` carries out, and the
//! erasure of what `auth` keeps of them (AUTH-023, the auth spec's "Deleting an account").

use platform::{
    authn::Principal,
    db::UnitOfWork,
    errors::Internal,
    events::{self, AccessChange},
    rate_limit::RateLimits,
    types::{Client, UserId},
};

use super::{Auth, buckets};
use crate::modules::{
    accounts::{self, DeletionOutcome},
    auth::{error::AuthError, repo},
};

impl Auth {
    /// Deletes the caller's account, once their session has proved their password within the
    /// last 5 minutes, through `accounts`, in one unit of work (AUTH-023), once the deletion has
    /// counted in the bucket of the account's email, which a read before the unit of work finds
    /// (AUTH-024, ARCH-11).
    pub(crate) async fn delete_account(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
    ) -> Result<(), AuthError> {
        let email = accounts::me(&self.database, client, principal)
            .await?
            .profile
            .email;
        limits
            .check(buckets::DELETION_EMAIL, email.as_str().as_bytes())
            .await?;
        let mut uow = self.database.begin(client).await?;
        let recent =
            repo::recently_authenticated(&mut uow, principal.user_id(), principal.session_id())
                .await?;
        if !recent {
            return Err(AuthError::ReauthenticationRequired);
        }
        match self.deletion.delete(&mut uow, principal).await? {
            DeletionOutcome::Deleted => Ok(uow.commit().await?),
            DeletionOutcome::OwnsSharedWorkspace => Err(AuthError::OwnsSharedWorkspace),
            DeletionOutcome::AccountChanged => Err(AuthError::AccountChanged),
        }
    }
}

/// Erases what `auth` keeps of `user_id`, whose account the unit of work deletes: their
/// identities first, which a sign-in locks before it starts a session, then their sessions, each
/// with the signal that closes its streams, then their device tokens and their verification codes
/// (the auth spec's "Deleting an account").
pub(in crate::modules::auth) async fn erase(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), Internal> {
    repo::delete_identities(uow, user_id).await?;
    for session_id in repo::delete_sessions(uow, user_id).await? {
        events::signal(
            uow,
            AccessChange::SessionEnded {
                user_id,
                session_id,
            },
        )
        .await?;
    }
    repo::delete_device_tokens(uow, user_id).await?;
    repo::codes::delete_all(uow, user_id).await
}
