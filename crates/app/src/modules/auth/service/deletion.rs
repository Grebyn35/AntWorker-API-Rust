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

use super::{Auth, buckets, reauthentication::require_recent_proof};
use crate::modules::{
    accounts::{self, DeletionOutcome},
    auth::{error::AuthError, repo},
};

impl Auth {
    /// Deletes the caller's account, once their session has proved who they are within the
    /// last 5 minutes, through `accounts`, in one unit of work (AUTH-023), once the deletion has
    /// counted in the bucket of the address that the account holds (AUTH-024): a read before the
    /// unit of work finds the address, and the unit of work reads it again once it holds the
    /// user's row. When the account holds another address then, the unit of work ends with
    /// nothing written, and the deletion counts in that address's bucket and reads again in a new
    /// one (ARCH-11). An account that another deletion took before the first read, or once the
    /// unit of work read the session's proof, is deleted already, as the deletion asked
    /// (ACCOUNTS-019); one that it took in between took the session too, which the proof's read
    /// answers with 401 (AUTH-023).
    pub(crate) async fn delete_account(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
    ) -> Result<(), AuthError> {
        let mut read = self.database.begin(client, Some(principal)).await?;
        let address = accounts::address(&mut read, principal.user_id()).await?;
        drop(read);
        let Some(accounts::Address {
            email: mut counted, ..
        }) = address
        else {
            return Ok(());
        };
        limits
            .check(buckets::DELETION_EMAIL, counted.as_str().as_bytes())
            .await?;
        loop {
            let mut uow = self.database.begin(client, Some(principal)).await?;
            require_recent_proof(&mut uow, principal).await?;
            // The user's row, locked as `Deletion::delete` then holds it, so the operation's locks
            // keep their order, with the address that the account holds once the lock is held.
            match accounts::address_for_update(&mut uow, principal.user_id()).await? {
                Some(held) if held.email != counted => {
                    // The address changed while the deletion waited for the row: the deletion
                    // counts in the bucket of the one the account holds, before it writes
                    // anything, and reads again (AUTH-024, ARCH-11).
                    drop(uow);
                    limits
                        .check(buckets::DELETION_EMAIL, held.email.as_str().as_bytes())
                        .await?;
                    counted = held.email;
                }
                _ => return self.delete_in(uow, principal).await,
            }
        }
    }

    /// Deletes the caller's account through `accounts` in `uow`, once `uow` has had its turn at
    /// their row, and commits, or answers why it doesn't (AUTH-023). An account that another
    /// deletion took while `uow` waited for the row is deleted already (ACCOUNTS-019).
    async fn delete_in(&self, mut uow: UnitOfWork, principal: &Principal) -> Result<(), AuthError> {
        match self.deletion.delete(&mut uow, principal).await? {
            DeletionOutcome::Deleted => Ok(uow.commit().await?),
            DeletionOutcome::OwnsSharedWorkspace => Err(AuthError::OwnsSharedWorkspace),
            DeletionOutcome::AccountChanged => Err(AuthError::AccountChanged),
        }
    }
}

/// Erases what `auth` keeps of `user_id`, whose account the unit of work deletes: their
/// identities first, which a sign-in and a sign-in's completion lock before they start a session,
/// then their authenticator app, its setup, their recovery codes and their sign-in challenges,
/// then their sessions, each with the signal that closes its streams, then their device tokens
/// and their verification codes (the auth spec's "Deleting an account").
pub(in crate::modules::auth) async fn erase(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), Internal> {
    repo::delete_identities(uow, user_id).await?;
    // The second factors before the sessions, the order of every operation's locks (the auth
    // spec's "A user's sessions").
    let _ = repo::second_factors::delete_app(uow, user_id).await?;
    repo::second_factors::delete_setup(uow, user_id).await?;
    repo::second_factors::delete_recovery_codes(uow, user_id).await?;
    repo::second_factors::delete_challenges(uow, user_id).await?;
    let ended: Vec<AccessChange> = repo::delete_sessions(uow, user_id)
        .await?
        .into_iter()
        .map(|session_id| AccessChange::SessionEnded {
            user_id,
            session_id,
        })
        .collect();
    events::signal(uow, &ended).await?;
    repo::delete_device_tokens(uow, user_id).await?;
    repo::codes::delete_all(uow, user_id).await
}
