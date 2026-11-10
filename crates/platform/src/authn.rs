//! Authentication: the `Principal` of a request, which the `SessionVerifier` that `auth`
//! implements proves from the request's access token, read from the `Authorization` header or a
//! browser's cookie (D18, D61), and the audit scope a proof gives an entry. A caller whose account
//! must still prove its email address is refused, but on the routes that proving it needs
//! (AUTH-025, D67).
// part: files
//! On a download route, the session that a signed URL names proves it instead (PLATFORM-080,
//! D113).
// end part: files

use std::{collections::HashSet, future::Future, pin::Pin, sync::Arc};

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
    response::{IntoResponse, Response},
};

use crate::{
    db::UnitOfWork,
    errors::{Internal, PlatformError, Problem, RequestError},
    http::Cookies,
    jobs::JobScope,
    rate_limit::{Bucket, LimitError, RateLimits},
    telemetry::names,
    types::{Client, SessionId, UserId, WorkspaceId},
};
// part: files
use crate::{
    http::signed::{self, Presented, TakesSignedUrl},
    signed_urls::SignedUrls,
};
// end part: files

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

    /// The scope of a job that the deletion of this caller's account enqueues once it has erased
    /// their data, such as the email that tells their address: no one's, since the user is gone,
    /// so no job names them (DATA-23, ACCOUNTS-015). Only the personal data contract of
    /// `accounts` calls it; clippy bans it elsewhere.
    pub fn erased_job_scope(&self) -> JobScope {
        JobScope::none()
    }

    /// The scope of the entry that records this caller leaving `workspace` as their account is
    /// deleted: the workspace's log, with them as the actor, whom deleting their row then sets to
    /// null (ACCOUNTS-014, AUDIT-006). Only the personal data contract of `workspaces` calls it;
    /// clippy bans it elsewhere.
    pub fn leaver_scope(&self, workspace: WorkspaceId) -> AuditScope {
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

    /// The log `log`, with no actor, for an entry that a job's run records for the user the job
    /// runs for, which only `JobContext` builds (DATA-22).
    pub(crate) fn unattributed(log: Log) -> Self {
        Self { log, actor: None }
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

/// What a credential proves: its principal, and whether the principal's account must still prove
/// its email address before the API serves it anything but proving it (AUTH-025, D67).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The caller.
    pub principal: Principal,
    /// Whether the deployment sends email and the caller's account hasn't proved its address.
    pub email_verification_required: bool,
}

/// The marker of a route that proving the caller's email address needs, such as signing out,
/// which a caller whose account must still prove it may call (AUTH-025).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Verifying;

/// Why a credential proves no principal.
#[derive(Debug)]
pub enum Unverified {
    /// No session has the credential, or its session doesn't last.
    Unknown,
    /// The credential's time has passed, as an expired access token's has.
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
    /// `client`, and whether its account must still prove its email address.
    fn verify<'a>(
        &'a self,
        token: &'a str,
        client: &'a Client,
    ) -> BoxFuture<'a, Result<Verified, Unverified>>;

    // part: files
    /// The principal of the session `session`, while it lasts, for a signed URL that acts for it
    /// until `until`, for a request from `client`: [`Unverified::Expired`] once `until` has
    /// passed, by the database's clock (DATA-03), and [`Unverified::Unknown`] for a session
    /// that doesn't last (AUTH-020), and whether its account must still prove its email address.
    fn session<'a>(
        &'a self,
        session: SessionId,
        until: time::OffsetDateTime,
        client: &'a Client,
    ) -> BoxFuture<'a, Result<Verified, Unverified>>;
    // end part: files

    /// The sessions of `principals` that still last, read inside `uow`: those that can still be
    /// refreshed, for the hub's re-check of its streams (SEC-06).
    fn lasting<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principals: &'a [Principal],
    ) -> BoxFuture<'a, Result<HashSet<SessionId>, Internal>>;
}

/// What authenticating a request takes: the verifier, and the user's rate-limit bucket.
#[derive(Clone)]
pub struct Authenticator {
    /// The verifier of access tokens.
    pub verifier: Arc<dyn SessionVerifier>,
    // part: files
    /// The checker of downloads' signed URLs (PLATFORM-080).
    pub signed: SignedUrls,
    // end part: files
    /// The bucket of each user's requests (PLATFORM-054).
    pub user_bucket: Bucket,
}

impl<S: Send + Sync> FromRequestParts<S> for Principal {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Some(authenticator) = parts.extensions.get::<Authenticator>().cloned() else {
            return Err(internal("the request carries no authenticator"));
        };
        let client = Client::from_request_parts(parts, state).await?;
        let verified = authenticator
            .verified(parts, &client)
            .await
            .map_err(|error| Problem(error).into_response())?;
        let principal = verified.principal;
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
        // Once the user's bucket counted the request, a caller whose account must still prove
        // its address is refused, but on a route that proving it needs (AUTH-025, PLATFORM-054).
        if verified.email_verification_required && parts.extensions.get::<Verifying>().is_none() {
            return Err(rejection(RequestError::EmailUnverified));
        }
        Ok(principal)
    }
}

impl Authenticator {
    /// What the request's credential proves, for a request from `client`.
    async fn verified(&self, parts: &Parts, client: &Client) -> Result<Verified, PlatformError> {
        // part: files
        if let Some(verified) = self.signed_session(parts, client).await? {
            return Ok(verified);
        }
        // end part: files
        let token = token(parts)?;
        self.verifier
            .verify(&token, client)
            .await
            .map_err(|unverified| refusal(unverified, RequestError::AccessTokenExpired))
    }

    // part: files
    /// What the session that the request's signed URL names proves, on a download route that
    /// takes one, or none when the request holds none of the URL's parameters, so that its
    /// credential proves it (PLATFORM-080).
    async fn signed_session(
        &self,
        parts: &Parts,
        client: &Client,
    ) -> Result<Option<Verified>, PlatformError> {
        if parts.extensions.get::<TakesSignedUrl>().is_none() {
            return Ok(None);
        }
        let signature = match signed::presented(parts) {
            Presented::None => return Ok(None),
            Presented::Malformed => return Err(RequestError::Unauthenticated.into()),
            Presented::Signed(signature) => signature,
        };
        let until = signature
            .expires_at()
            .filter(|_| self.signed.matches(parts.uri.path(), &signature))
            .ok_or(RequestError::Unauthenticated)?;
        self.verifier
            .session(signature.session(), until, client)
            .await
            .map(Some)
            .map_err(|unverified| refusal(unverified, RequestError::SignedUrlExpired))
    }
    // end part: files
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

/// The refusal of a credential that proves no principal, `expired` being the one for a
/// credential past its time.
fn refusal(unverified: Unverified, expired: RequestError) -> PlatformError {
    match unverified {
        Unverified::Unknown => RequestError::Unauthenticated.into(),
        Unverified::Expired => expired.into(),
        Unverified::Failed(error) => error.into(),
    }
}

fn internal(reason: &'static str) -> Response {
    Problem(Internal::new(reason)).into_response()
}
