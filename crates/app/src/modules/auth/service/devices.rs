//! Device tokens: whether a sign-in's is valid for its email, issuing the next, and the name of
//! the cookie that holds a browser's (AUTH-015, AUTH-019).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    hashing::sha256,
    random,
    types::{Client, EmailAddress, Secret, UserId},
};

use super::{Auth, SignIn};
use crate::modules::auth::{
    model::{DEVICE_COOKIE, DeviceToken},
    repo,
};

/// The kind of the keyed hash of a device token's email.
const DEVICE_TOKEN_EMAIL: &str = "device_token_email";

/// The kind of the keyed hash that names the cookie of an email's device token.
const DEVICE_TOKEN_COOKIE: &str = "device_token_cookie";

/// How many bytes of its keyed hash a cookie's name holds: 64 bits, so that no two of the
/// accounts a browser signs in to share a cookie.
const COOKIE_HASH_BYTES: usize = 8;

impl Auth {
    /// The hash of the sign-in's device token, from its body, or else from the cookie of its
    /// email, when it is valid for the email, read before any limit counts the attempt, since it
    /// decides which limit does, in a unit of work of its own that writes nothing (ARCH-11,
    /// AUTH-015, AUTH-019).
    pub(super) async fn trusted(
        &self,
        client: &Client,
        input: &SignIn,
    ) -> Result<Option<[u8; 32]>, Internal> {
        let Some(token) = self.brought(&input.email, &input.device_token, &input.device_cookies)
        else {
            return Ok(None);
        };
        let mut uow = self.database.begin(client).await?;
        let valid = self.valid(&mut uow, &input.email, token).await?;
        drop(uow);
        Ok(valid)
    }

    /// The hash of the device token that a sign-in through a provider brought, from its body, or
    /// else from the cookie of the account's `email`, when it is valid for the email, read in the
    /// sign-in's unit of work, once the account is known (AUTH-019, AUTH-073).
    pub(super) async fn trusted_in(
        &self,
        uow: &mut UnitOfWork,
        email: &EmailAddress,
        device_token: &Option<Secret<String>>,
        cookies: &[(String, Secret<String>)],
    ) -> Result<Option<[u8; 32]>, Internal> {
        match self.brought(email, device_token, cookies) {
            Some(token) => self.valid(uow, email, token).await,
            None => Ok(None),
        }
    }

    /// The device token that a sign-in of `email` brought: `device_token`, or else the one of
    /// `cookies` named for the email (AUTH-015).
    fn brought<'a>(
        &self,
        email: &EmailAddress,
        device_token: &'a Option<Secret<String>>,
        cookies: &'a [(String, Secret<String>)],
    ) -> Option<&'a Secret<String>> {
        let cookie = self.device_cookie(email);
        device_token.as_ref().or_else(|| {
            cookies
                .iter()
                .find(|(name, _)| *name == cookie)
                .map(|(_, token)| token)
        })
    }

    /// The hash of `token`, when it is a device token of `email` that hasn't expired.
    async fn valid(
        &self,
        uow: &mut UnitOfWork,
        email: &EmailAddress,
        token: &Secret<String>,
    ) -> Result<Option<[u8; 32]>, Internal> {
        let hash = sha256(token.expose().as_bytes());
        let email = self.key.hash(DEVICE_TOKEN_EMAIL, email.as_str().as_bytes());
        Ok(repo::device_token_valid(uow, &hash, &email)
            .await?
            .then_some(hash))
    }

    /// Issues `user_id`'s device a device token for `email`, ending the account's expired ones,
    /// and the one that hashes to `replaced`, if any, the token the sign-in used (AUTH-019).
    pub(super) async fn issue_device_token(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
        email: &EmailAddress,
        replaced: Option<&[u8; 32]>,
    ) -> Result<DeviceToken, Internal> {
        repo::end_device_tokens(uow, user_id, replaced.map(<[u8; 32]>::as_slice)).await?;
        let token = Secret::new(random::token()?);
        let (id, expires_at) = repo::insert_device_token(
            uow,
            user_id,
            &self.key.hash(DEVICE_TOKEN_EMAIL, email.as_str().as_bytes()),
            &sha256(token.expose().as_bytes()),
            self.settings.device_tokens,
        )
        .await?;
        Ok(DeviceToken {
            id,
            cookie: self.device_cookie(email),
            token,
            expires_at,
        })
    }

    /// The name of the cookie, after `__Host-`, that holds a browser's device token for `email`:
    /// one for each account the browser signs in to, so that signing in to one leaves another's
    /// (AUTH-015). It holds part of a keyed hash of the email, so it shows no email.
    fn device_cookie(&self, email: &EmailAddress) -> String {
        let hash = self
            .key
            .hash(DEVICE_TOKEN_COOKIE, email.as_str().as_bytes());
        let hex: String = hash
            .iter()
            .take(COOKIE_HASH_BYTES)
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("{DEVICE_COOKIE}{hex}")
    }
}
