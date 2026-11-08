//! The verifier of access tokens and of signed URLs' sessions, which `platform::authn` asks to
//! prove each protected request's principal (AUTH-009, AUTH-018, AUTH-020).

use std::{collections::HashSet, sync::Arc};

use platform::{
    authn::{BoxFuture, Principal, SessionVerifier, Unverified},
    db::UnitOfWork,
    errors::Internal,
    hashing::sha256,
    types::{Client, SessionId, UserId},
};

use super::Auth;
use crate::modules::auth::repo;

/// Proves the principal of an access token for `platform::authn` (AUTH-009).
pub(crate) struct Verifier(pub(crate) Arc<Auth>);

impl SessionVerifier for Verifier {
    fn verify<'a>(
        &'a self,
        token: &'a str,
        client: &'a Client,
    ) -> BoxFuture<'a, Result<Principal, Unverified>> {
        Box::pin(async move {
            let mut uow = self
                .0
                .database
                .begin(client)
                .await
                .map_err(Unverified::Failed)?;
            let found = repo::by_access_token(&mut uow, &sha256(token.as_bytes()))
                .await
                .map_err(Unverified::Failed)?
                .ok_or(Unverified::Unknown)?;
            if found.expired {
                return Err(Unverified::Expired);
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "auth builds the principal of a verified session (the auth spec)"
            )]
            let principal = Principal::new(found.user_id, found.id);
            Ok(principal)
        })
    }

    fn session<'a>(
        &'a self,
        session: SessionId,
        until: time::OffsetDateTime,
        client: &'a Client,
    ) -> BoxFuture<'a, Result<Principal, Unverified>> {
        Box::pin(async move {
            let mut uow = self
                .0
                .database
                .begin(client)
                .await
                .map_err(Unverified::Failed)?;
            let (user_id, passed) = repo::lasting_session(&mut uow, session, until)
                .await
                .map_err(Unverified::Failed)?
                .ok_or(Unverified::Unknown)?;
            if passed {
                return Err(Unverified::Expired);
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "auth builds the principal of a session that lasts (the auth spec)"
            )]
            let principal = Principal::new(user_id, session);
            Ok(principal)
        })
    }

    fn lasting<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principals: &'a [Principal],
    ) -> BoxFuture<'a, Result<HashSet<SessionId>, Internal>> {
        Box::pin(async move {
            let sessions: Vec<(UserId, SessionId)> = principals
                .iter()
                .map(|principal| (principal.user_id(), principal.session_id()))
                .collect();
            Ok(repo::lasting(uow, &sessions).await?.into_iter().collect())
        })
    }
}
