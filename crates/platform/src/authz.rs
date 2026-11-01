//! Authorization: the proof `Authorized<P>` that the caller is a member of the workspace in the
//! path and holds permission `P`, which the `AccessPolicy` that `access` implements decides
//! inside the request's unit of work (D2, PLATFORM-024), and the report of every refusal to the
//! `SecurityEventSink` that `audit` implements (ACCESS-003, PLATFORM-025).

use std::{marker::PhantomData, sync::Arc};

use axum::{
    extract::{FromRequestParts, RawPathParams},
    http::request::Parts,
    response::{IntoResponse, Response},
};

use crate::{
    authn::{AuditScope, BoxFuture, Log, Principal},
    db::{Database, UnitOfWork},
    errors::{Internal, PlatformError, Problem, RequestError},
    telemetry::{metrics::Counter, names},
    types::{Client, UserId, WorkspaceId},
};

/// The path parameter that names a route's workspace (API-02).
const WORKSPACE_PARAMETER: &str = "workspace_id";

/// A permission of the matrix in `access`, one type per permission, which a workspace route
/// requires through `Authorize<P>` (API-14).
pub trait Permission: Send + Sync + 'static {
    /// The permission's name, such as `members.read`.
    const NAME: &'static str;
}

/// What `AccessPolicy::authorize` found ("Refusals" in the platform spec).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// The caller isn't a member of the workspace, or it doesn't exist.
    NotMember,
    /// The caller is a member whose role lacks the permission.
    Denied,
    /// The caller is a member with the permission, at the role of this name.
    Allowed(&'static str),
}

/// Decides whether a caller may act in a workspace, which `access` implements for `platform`
/// (ARCH-13).
pub trait AccessPolicy: Send + Sync {
    /// Whether `user` holds `permission` in `workspace`, read inside `uow`. A member's row stays
    /// locked until `uow` ends, so the operation runs under the role it was authorized with
    /// (PLATFORM-024).
    fn authorize<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        user: UserId,
        workspace: WorkspaceId,
        permission: &'static str,
    ) -> BoxFuture<'a, Result<Decision, Internal>>;
}

/// A refused request, as the `SecurityEventSink` records it (DATA-27).
#[derive(Clone, Debug)]
pub struct Refusal {
    user_id: UserId,
    workspace_id: WorkspaceId,
    permission: &'static str,
    member: bool,
    client: Client,
}

impl Refusal {
    /// The workspace the caller was refused in.
    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// The permission the request needed.
    pub fn permission(&self) -> &'static str {
        self.permission
    }

    /// The client of the refused request, whose entry records it (AUDIT-003).
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Where the refusal is recorded, with the caller as the actor: a member's in the
    /// workspace's log, and a non-member's in their own, so a workspace's log never names who
    /// probed it (AUDIT-004).
    pub fn audit_scope(&self) -> AuditScope {
        let log = if self.member {
            Log::Workspace(self.workspace_id)
        } else {
            Log::User(self.user_id)
        };
        AuditScope::proven(log, self.user_id)
    }
}

/// Records refused requests, which `audit` implements for `platform` (ARCH-13).
pub trait SecurityEventSink: Send + Sync {
    /// Records `refusal`, in a transaction of its own.
    fn refused<'a>(&'a self, refusal: &'a Refusal) -> BoxFuture<'a, Result<(), Internal>>;
}

/// What authorizing a workspace request takes: the database the unit of work opens on, the
/// policy, and the sink of refusals.
#[derive(Clone)]
pub struct Authorizer {
    /// The database of the request's unit of work.
    pub database: Database,
    /// The policy that `access` implements.
    pub policy: Arc<dyn AccessPolicy>,
    /// The sink that `audit` implements.
    pub sink: Arc<dyn SecurityEventSink>,
}

/// Why a request was refused, as `platform.authorization_refusals` counts it.
#[derive(Clone, Copy, Debug)]
enum Reason {
    NotMember,
    Permission,
    Rank,
}

static REFUSALS: Counter<1> = Counter::new(
    "platform.authorization_refusals",
    "{refusal}",
    "Requests that authorization refused, by why: not_member, permission or rank (ACCESS-003, \
     ACCESS-004).",
    [names::AUTHORIZATION_REASON],
);

/// The extractor of a workspace route: the caller and the workspace in the path, which proves
/// nothing until [`Authorize::open`]. The handler calls it once the body is parsed, so the unit
/// of work opens last (architecture §6).
pub struct Authorize<P> {
    principal: Principal,
    workspace_id: Option<WorkspaceId>,
    client: Client,
    authorizer: Authorizer,
    permission: PhantomData<fn() -> P>,
}

impl<P: Permission, S: Send + Sync> FromRequestParts<S> for Authorize<P> {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Some(authorizer) = parts.extensions.get::<Authorizer>().cloned() else {
            return Err(Problem(Internal::new("the request carries no authorizer")).into_response());
        };
        let principal = Principal::from_request_parts(parts, state).await?;
        let client = Client::from_request_parts(parts, state).await?;
        // A path segment that isn't a UUID names no workspace, which answers as one the caller
        // doesn't belong to.
        let workspace_id = RawPathParams::from_request_parts(parts, state)
            .await
            .ok()
            .and_then(|params| {
                params
                    .iter()
                    .find(|(name, _)| *name == WORKSPACE_PARAMETER)
                    .and_then(|(_, value)| uuid::Uuid::parse_str(value).ok())
            })
            .map(WorkspaceId::from_uuid);
        Ok(Self {
            principal,
            workspace_id,
            client,
            authorizer,
            permission: PhantomData,
        })
    }
}

impl<P: Permission> Authorize<P> {
    /// The caller.
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// Opens the request's unit of work and authorizes the caller inside it: the open unit of
    /// work with the proof, or 404 `request.not_found` for a caller who isn't a member and 403
    /// `request.permission_denied` for a member without `P`, each recorded first (ACCESS-003,
    /// PLATFORM-024).
    pub async fn open(self) -> Result<(UnitOfWork, Authorized<P>), PlatformError> {
        let Some(workspace_id) = self.workspace_id else {
            return Err(RequestError::NotFound.into());
        };
        let mut uow = self.authorizer.database.begin(&self.client).await?;
        let decision = self
            .authorizer
            .policy
            .authorize(&mut uow, self.principal.user_id(), workspace_id, P::NAME)
            .await?;
        let refusal = |member| Refusal {
            user_id: self.principal.user_id(),
            workspace_id,
            permission: P::NAME,
            member,
            client: self.client.clone(),
        };
        match decision {
            Decision::Allowed(role) => {
                // The request's span carries the workspace the caller was authorized in (OBS-09).
                tracing::Span::current().record(names::WORKSPACE_ID, workspace_id.to_string());
                Ok((
                    uow,
                    Authorized {
                        principal: self.principal,
                        workspace_id,
                        role,
                        client: self.client,
                        sink: self.authorizer.sink,
                        permission: PhantomData,
                    },
                ))
            }
            Decision::NotMember => {
                drop(uow);
                report(&*self.authorizer.sink, &refusal(false), Reason::NotMember).await?;
                Err(RequestError::NotFound.into())
            }
            Decision::Denied => {
                drop(uow);
                report(&*self.authorizer.sink, &refusal(true), Reason::Permission).await?;
                Err(RequestError::PermissionDenied.into())
            }
        }
    }
}

/// The proof that the caller is a member of a workspace and holds permission `P` there, built
/// only by [`Authorize::open`] (D2). The operation runs in the unit of work it was proven in.
pub struct Authorized<P> {
    principal: Principal,
    workspace_id: WorkspaceId,
    role: &'static str,
    client: Client,
    sink: Arc<dyn SecurityEventSink>,
    permission: PhantomData<fn() -> P>,
}

impl<P: Permission> Authorized<P> {
    /// The caller.
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// The workspace the caller is a member of, from the path (API-02).
    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// The name of the caller's role in the workspace, which `access` reads back into its role.
    pub fn role(&self) -> &'static str {
        self.role
    }

    /// The scope of an audit entry this member records: the workspace's log, with them as the
    /// actor (AUDIT-002).
    pub fn audit_scope(&self) -> AuditScope {
        AuditScope::proven(Log::Workspace(self.workspace_id), self.principal.user_id())
    }
}

/// Refuses an operation that found its member may not act on another member of the same or a
/// higher rank (SEC-08): rolls `uow` back, records the refusal, and answers 403
/// `request.permission_denied` (PLATFORM-025).
pub async fn refuse<P: Permission>(uow: UnitOfWork, authorized: &Authorized<P>) -> PlatformError {
    drop(uow);
    let refusal = Refusal {
        user_id: authorized.principal.user_id(),
        workspace_id: authorized.workspace_id,
        permission: P::NAME,
        member: true,
        client: authorized.client.clone(),
    };
    match report(&*authorized.sink, &refusal, Reason::Rank).await {
        Ok(()) => RequestError::PermissionDenied.into(),
        Err(error) => error.into(),
    }
}

/// Records `refusal` and counts it. A refusal the sink can't record fails the request, so none
/// goes unrecorded (DATA-27).
async fn report(
    sink: &dyn SecurityEventSink,
    refusal: &Refusal,
    reason: Reason,
) -> Result<(), Internal> {
    let label = match reason {
        Reason::NotMember => "not_member",
        Reason::Permission => "permission",
        Reason::Rank => "rank",
    };
    REFUSALS.add(1, [label.into()]);
    sink.refused(refusal).await
}
