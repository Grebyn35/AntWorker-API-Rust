//! Authentication: the `Principal` of a request, which the `SessionVerifier` that `auth`
//! implements proves from the request's access token, read from the `Authorization` header or a
//! browser's cookie (D18, D61), or, on a download route, from the session that its signed URL
//! names (PLATFORM-080, D113), and the audit scope a proof gives an entry.

use std::{collections::HashSet, future::Future, pin::Pin, sync::Arc};

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
    response::{IntoResponse, Response},
};

use crate::{
    db::UnitOfWork,
    errors::{Internal, Problem, RequestError},
    http::{
        Cookies,
        signed::{self, Presented, TakesSignedUrl},
    },
    jobs::JobScope,
    rate_limit::{Bucket, LimitError, RateLimits},
    signed_urls::SignedUrls,
    telemetry::names,
    types::{Client, SessionId, UserId, WorkspaceId},
};

/// The name of the cookie that holds a browser's access token, after `__Host-` (AUTH-015).
pub const ACCESS_COOKIE: &str = "access_token";

/// The authenticated caller of a request: a user, through one of their sessions. Only `auth`
/// builds one, after a successful credential check or a registration; clippy bans
/// [`Principal::new`] elsewhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    user_id: UserId,
    session_id: SessionId,
}

impl Principal {
    /// The principal of `user_id`, signed in through `session_id`.
    pub fn new(user_id: UserId, session_id: SessionId) -> Self {
        Self {
            user_id,
            session_id,
        }
    }

    /// The caller.
    pub fn user_id(&self) -> UserId {
        self.user_id
    }

    /// The session the caller signed in through.
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// The scope of an audit entry this caller records: their own log, with them as the actor.
    pub fn audit_scope(&self) -> AuditScope {
        AuditScope {
            log: Log::User(self.user_id),
            actor: Some(self.user_id),
        }
    }

    /// The scope of a job this caller's operation enqueues: their own (PLATFORM-013).
    pub fn job_scope(&self) -> JobScope {
        JobScope::proven(Some(self.user_id), None)
    }

    /// The scope of the entry that records this caller creating `workspace`, whose ID the
    /// database just gave it: the workspace's log, with them as the actor, before any
    /// `Authorized<P>` can name it (WORKSPACES-001). Only the operation that creates a workspace
    /// calls it; clippy bans it elsewhere.
    pub fn founder_scope(&self, workspace: WorkspaceId) -> AuditScope {
        AuditScope {
            log: Log::Workspace(workspace),
            actor: Some(self.user_id),
        }
    }

    // part: invitations
    /// The scope of the entry that records this caller joining `workspace` by accepting an
    /// invitation addressed to them: the workspace's log, with them as the actor, before any
    /// `Authorized<P>` can name it (INVITATIONS-005). Only accepting an invitation calls it;
    /// clippy bans it elsewhere.
    pub fn joiner_scope(&self, workspace: WorkspaceId) -> AuditScope {
        AuditScope {
            log: Log::Workspace(workspace),
            actor: Some(self.user_id),
        }
    }
    // end part: invitations
}

/// The log an audit entry belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Log {
    /// A user's own log.
    User(UserId),
    /// A workspace's log.
    Workspace(WorkspaceId),
}

/// The log and the actor of an audit entry. Only a proof builds one with an actor, so no module
/// records an entry as someone the request didn't prove it is (AUDIT-002).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuditScope {
    log: Log,
    actor: Option<UserId>,
}

impl AuditScope {
    /// The log of the account that an unauthenticated request acted on, such as with a failed
    /// sign-in, with no actor.
    pub fn attempt(user_id: UserId) -> Self {
        Self {
            log: Log::User(user_id),
            actor: None,
        }
    }

    /// The log `log`, with `actor` as the actor, which only the proofs of `platform` build.
    pub(crate) fn proven(log: Log, actor: UserId) -> Self {
        Self {
            log,
            actor: Some(actor),
        }
    }

    /// The log the entry belongs to.
    pub fn log(&self) -> Log {
        self.log
    }

    /// The user who acted, when a proof names one.
    pub fn actor(&self) -> Option<UserId> {
        self.actor
    }
}

/// Why an access token, or a signed URL's session, proves no principal.
#[derive(Debug)]
pub enum Unverified {
    /// No session has the token, or the signed URL's session doesn't last.
    Unknown,
    /// The session's access token has expired, or the signed URL's time has passed.
    Expired,
    /// The check failed.
    Failed(Internal),
}

/// A boxed future, so `platform` can hold a verifier without knowing its type.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Verifies an access token and proves the principal it belongs to, which `auth` implements for
/// `platform` (ARCH-13).
pub trait SessionVerifier: Send + Sync {
    /// The principal whose session holds `token` as its access token, for a request from
    /// `client`.
    fn verify<'a>(
        &'a self,
        token: &'a str,
        client: &'a Client,
    ) -> BoxFuture<'a, Result<Principal, Unverified>>;

    /// The principal of the session `session`, while it lasts, for a signed URL that acts for it
    /// until `until`, for a request from `client`: [`Unverified::Expired`] once `until` has
    /// passed, by the database's clock (DATA-03), and [`Unverified::Unknown`] for a session
    /// that doesn't last (AUTH-020).
    fn session<'a>(
        &'a self,
        session: SessionId,
        until: time::OffsetDateTime,
        client: &'a Client,
    ) -> BoxFuture<'a, Result<Principal, Unverified>>;

    /// The sessions of `principals` that still last, read inside `uow`: those that can still be
    /// refreshed, for the hub's re-check of its streams (SEC-06).
    fn lasting<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principals: &'a [Principal],
    ) -> BoxFuture<'a, Result<HashSet<SessionId>, Internal>>;
}

/// What authenticating a request takes: the verifier, the checker of signed URLs, and the
/// user's rate-limit bucket.
#[derive(Clone)]
pub struct Authenticator {
    /// The verifier of access tokens.
    pub verifier: Arc<dyn SessionVerifier>,
    /// The checker of downloads' signed URLs (PLATFORM-080).
    pub signed: SignedUrls,
    /// The bucket of each user's requests (PLATFORM-054).
    pub user_bucket: Bucket,
}

impl<S: Send + Sync> FromRequestParts<S> for Principal {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Some(authenticator) = parts.extensions.get::<Authenticator>().cloned() else {
            return Err(internal("the request carries no authenticator"));
        };
        let presented = if parts.extensions.get::<TakesSignedUrl>().is_some() {
            signed::presented(parts)
        } else {
            Presented::None
        };
        let principal = match presented {
            Presented::None => {
                let token = token(parts).map_err(rejection)?;
                let client = Client::from_request_parts(parts, state).await?;
                authenticator
                    .verifier
                    .verify(&token, &client)
                    .await
                    .map_err(|unverified| {
                        unverified_rejection(unverified, RequestError::AccessTokenExpired)
                    })?
            }
            Presented::Malformed => return Err(rejection(RequestError::Unauthenticated)),
            Presented::Signed(signature) => {
                let until = signature
                    .expires_at()
                    .filter(|_| authenticator.signed.matches(parts.uri.path(), &signature))
                    .ok_or_else(|| rejection(RequestError::Unauthenticated))?;
                let client = Client::from_request_parts(parts, state).await?;
                authenticator
                    .verifier
                    .session(signature.session(), until, &client)
                    .await
                    .map_err(|unverified| {
                        unverified_rejection(unverified, RequestError::SignedUrlExpired)
                    })?
            }
        };
        // The request's span carries the caller the session proved (OBS-09).
        tracing::Span::current().record(names::USER_ID, principal.user_id.to_string());
        let limits = RateLimits::from_request_parts(parts, state).await?;
        let user = principal.user_id.uuid();
        limits
            .check(authenticator.user_bucket, user.as_bytes())
            .await
            .map_err(|error| match error {
                LimitError::RateLimited => rejection(RequestError::RateLimited),
                LimitError::Internal(error) => Problem(error).into_response(),
            })?;
        Ok(principal)
    }
}

/// The request's access token: from the `Authorization` header, which must then be `Bearer` and
/// a token, or, without one, from the access cookie (AUTH-009).
fn token(parts: &Parts) -> Result<String, RequestError> {
    if let Some(value) = parts.headers.get(header::AUTHORIZATION) {
        return value
            .to_str()
            .ok()
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, token)| scheme.eq_ignore_ascii_case("bearer") && !token.is_empty())
            .map(|(_, token)| token.trim().to_owned())
            .ok_or(RequestError::Unauthenticated);
    }
    match Cookies::of(parts).credential(ACCESS_COOKIE)? {
        Some(token) if !token.is_empty() => Ok(token.to_owned()),
        _ => Err(RequestError::Unauthenticated),
    }
}

fn rejection(error: RequestError) -> Response {
    Problem(error).into_response()
}

/// The answer to a credential that proves no principal, `expired` being the one for a credential
/// past its time.
fn unverified_rejection(unverified: Unverified, expired: RequestError) -> Response {
    match unverified {
        Unverified::Unknown => rejection(RequestError::Unauthenticated),
        Unverified::Expired => rejection(expired),
        Unverified::Failed(error) => Problem(error).into_response(),
    }
}

fn internal(reason: &'static str) -> Response {
    Problem(Internal::new(reason)).into_response()
}
