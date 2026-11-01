//! Authentication: the `Principal` of a request, which the `SessionVerifier` that `auth`
//! implements proves from the request's access token, read from the `Authorization` header or a
//! browser's cookie (D18, D61), and the audit scope a proof gives an entry.

use std::{future::Future, pin::Pin, sync::Arc};

use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts},
    response::{IntoResponse, Response},
};

use crate::{
    errors::{Internal, Problem, RequestError},
    http::Cookies,
    jobs::JobScope,
    rate_limit::{Bucket, LimitError, RateLimits},
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

    /// The scope of a job this caller's operation enqueues: their own (PLATFORM-012).
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

/// Why an access token proves no principal.
#[derive(Debug)]
pub enum Unverified {
    /// No session has the token.
    Unknown,
    /// The session's access token has expired.
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
}

/// What authenticating a request takes: the verifier, and the user's rate-limit bucket.
#[derive(Clone)]
pub struct Authenticator {
    /// The verifier of access tokens.
    pub verifier: Arc<dyn SessionVerifier>,
    /// The bucket of each user's requests (PLATFORM-048).
    pub user_bucket: Bucket,
}

impl<S: Send + Sync> FromRequestParts<S> for Principal {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Some(authenticator) = parts.extensions.get::<Authenticator>().cloned() else {
            return Err(internal("the request carries no authenticator"));
        };
        let token = token(parts).map_err(rejection)?;
        let client = Client::from_request_parts(parts, state).await?;
        let principal = authenticator
            .verifier
            .verify(&token, &client)
            .await
            .map_err(|unverified| match unverified {
                Unverified::Unknown => rejection(RequestError::Unauthenticated),
                Unverified::Expired => rejection(RequestError::AccessTokenExpired),
                Unverified::Failed(error) => Problem(error).into_response(),
            })?;
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

fn internal(reason: &'static str) -> Response {
    Problem(Internal::new(reason)).into_response()
}
