//! Device tokens: whether a sign-in's is valid for its email, and issuing the next (AUTH-019).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    hashing::sha256,
    random,
    types::{Client, EmailAddress, Secret, UserId},
};

use super::{Auth, SignIn};
use crate::modules::auth::{model::DeviceToken, repo};

/// The kind of the keyed hash of a device token's email.
const DEVICE_TOKEN_EMAIL: &str = "device_token_email";

impl Auth {
    /// The hash of the sign-in's device token, when it is valid for its email, read in a unit of
    /// work of its own, before any limit counts the attempt (AUTH-019).
    pub(super) async fn trusted(
        &self,
        client: &Client,
        input: &SignIn,
    ) -> Result<Option<[u8; 32]>, Internal> {
        let Some(token) = &input.device_token else {
            return Ok(None);
        };
        let hash = sha256(token.expose().as_bytes());
        let email = self
            .key
            .hash(DEVICE_TOKEN_EMAIL, input.email.as_str().as_bytes());
        let mut uow = self.database.begin(client).await?;
        let valid = repo::device_token_valid(&mut uow, &hash, &email).await?;
        drop(uow);
        Ok(valid.then_some(hash))
    }

    /// Issues `user_id`'s device a device token for `email`, in place of the one that hashes to
    /// `replaced`, if any, the token the sign-in used (AUTH-019).
    pub(super) async fn issue_device_token(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        email: &EmailAddress,
        replaced: Option<&[u8; 32]>,
    ) -> Result<DeviceToken, Internal> {
        let token = Secret::new(random::token()?);
        let expires_at = repo::issue_device_token(
            uow,
            user_id,
            &self.key.hash(DEVICE_TOKEN_EMAIL, email.as_str().as_bytes()),
            &sha256(token.expose().as_bytes()),
            replaced.map(<[u8; 32]>::as_slice),
            self.settings.device_tokens,
        )
        .await?;
        Ok(DeviceToken { token, expires_at })
    }
}
