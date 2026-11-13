//! Authorization: the proof `Authorized<P>` that the caller is a member of the workspace in the
//! path and holds permission `P`, which the `AccessPolicy` that `access` implements decides
//! inside the request's unit of work (D2, PLATFORM-009), and the report of every refusal to the
//! `SecurityEventSink` that `audit` implements (ACCESS-003, PLATFORM-010).

use std::{
    collections::{BTreeMap, HashMap},
    marker::PhantomData,
    sync::Arc,
};

use axum::{
    extract::{FromRequestParts, RawPathParams},
    http::request::Parts,
    response::{IntoResponse, Response},
};

use crate::{
    authn::{AuditScope, BoxFuture, Log, Principal},
    db::{Database, UnitOfWork},
    errors::{Internal, PlatformError, Problem, RequestError},
    http::path_id,
    jobs::JobScope,
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
    /// (PLATFORM-009).
    fn authorize<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        user: UserId,
        workspace: WorkspaceId,
        permission: &'static str,
    ) -> BoxFuture<'a, Result<Decision, Internal>>;

    /// The memberships of each of `users`, read inside `uow`, for the hub's streams: a user who
    /// belongs to no workspace may be left out (SEC-19).
    fn memberships<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        users: &'a [UserId],
    ) -> BoxFuture<'a, Result<HashMap<UserId, Memberships>, Internal>>;
}

/// The workspaces a user belongs to, each with the names of the permissions their role holds
/// there and the transaction that added them, which `AccessPolicy::memberships` reads for the
/// hub, so a stream receives only the workspace events its caller may, and of those only the ones
/// that committed after their membership did (SEC-19).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Memberships {
    workspaces: BTreeMap<WorkspaceId, Membership>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Membership {
    permissions: Vec<&'static str>,
    joined: u64,
}

impl Memberships {
    /// Records that the user belongs to `workspace`, with a role that holds `permissions`, since
    /// the transaction `joined` committed: its ID, as `pg_current_xact_id()` gave it.
    pub fn insert(&mut self, workspace: WorkspaceId, joined: u64, permissions: &[&'static str]) {
        let mut permissions = permissions.to_vec();
        permissions.sort_unstable();
        self.workspaces.insert(
            workspace,
            Membership {
                permissions,
                joined,
            },
        );
    }

    /// The workspaces the user belongs to.
    pub(crate) fn workspaces(&self) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.workspaces.keys().copied()
    }

    /// Whether an event of `workspace` that requires `permission`, and committed when `committed`
    /// had, reaches the user: their role there holds the permission, and their membership had
    /// committed before the event did.
    pub(crate) fn admits(
        &self,
        workspace: WorkspaceId,
        permission: &str,
        committed: &Committed,
    ) -> bool {
        self.workspaces.get(&workspace).is_some_and(|membership| {
            membership.permissions.contains(&permission) && committed.includes(membership.joined)
        })
    }

    /// One row per workspace and permission the user holds there: its workspace, the
    /// permission, and the transaction that added the membership, as text for `xid8`, which the
    /// replay's query joins on.
    pub(crate) fn rows(&self) -> (Vec<uuid::Uuid>, Vec<String>, Vec<String>) {
        let mut rows = (Vec::new(), Vec::new(), Vec::new());
        for (workspace, membership) in &self.workspaces {
            for permission in &membership.permissions {
                rows.0.push(workspace.uuid());
                rows.1.push((*permission).to_owned());
                rows.2.push(membership.joined.to_string());
            }
        }
        rows
    }
}

/// The transactions a `pg_snapshot` holds as completed: those below `xmin`, and those below `xmax`
/// that weren't running, as Postgres's `pg_visible_in_snapshot` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    xmin: u64,
    xmax: u64,
    running: Vec<u64>,
}

impl Snapshot {
    /// The snapshot whose text form, `xmin:xmax:xip,...`, is `text`, or `None` for text that
    /// isn't one.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let (Some(xmin), Some(xmax), Some(running), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let running = running
            .split(',')
            .filter(|id| !id.is_empty())
            .map(str::parse)
            .collect::<Result<Vec<u64>, _>>()
            .ok()?;
        Some(Self {
            xmin: xmin.parse().ok()?,
            xmax: xmax.parse().ok()?,
            running,
        })
    }

    /// Whether the transaction `id` had completed when the snapshot was taken.
    pub(crate) fn includes(&self, id: u64) -> bool {
        id < self.xmin || (id < self.xmax && !self.running.contains(&id))
    }
}

/// The transactions that had committed when an event did: those of the snapshot its commit took,
/// and the event's own, which a snapshot leaves out, but which commits with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Committed {
    own: u64,
    snapshot: Snapshot,
}

impl Committed {
    /// The transactions of the event whose own transaction is `own`, and whose commit took
    /// `snapshot`, a `pg_snapshot` in its text form, or `None` for text that isn't one.
    pub(crate) fn parse(own: &str, snapshot: &str) -> Option<Self> {
        Some(Self {
            own: own.parse().ok()?,
            snapshot: Snapshot::parse(snapshot)?,
        })
    }

    /// Whether the transaction `id` had committed, or is the event's own.
    fn includes(&self, id: u64) -> bool {
        id == self.own || self.snapshot.includes(id)
    }
}

/// A refused request, as the `SecurityEventSink` records it (DATA-22).
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

    /// Where a member's refusal is recorded once the workspace's log is gone, because the
    /// workspace was deleted while the refused request ran: the caller's own log, as a
    /// non-member's is, which they then are (AUDIT-004).
    pub fn own_scope(&self) -> AuditScope {
        AuditScope::proven(Log::User(self.user_id), self.user_id)
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
            .and_then(|params| path_id(&params, WORKSPACE_PARAMETER))
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
    /// The authenticated caller, before anything authorizes them, such as for a per-action
    /// limit that counts before the unit of work opens (PLATFORM-011).
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// Opens the request's unit of work and authorizes the caller inside it: the open unit of
    /// work with the proof, or 404 `request.not_found` for a caller who isn't a member and 403
    /// `request.permission_denied` for a member without `P`, each recorded first (ACCESS-003,
    /// PLATFORM-009).
    pub async fn open(self) -> Result<(UnitOfWork, Authorized<P>), PlatformError> {
        let (uow, workspace_id, decision) = self.decide().await?;
        let Decision::Allowed(role) = decision else {
            drop(uow);
            return Err(self.refuse(workspace_id, decision).await);
        };
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

    // part: files
    /// Authorizes the caller as [`Authorize::open`] does, refusals and their records included,
    /// in a unit of work of its own that commits nothing, for a route that refuses a caller
    /// before work it does outside any transaction, such as reading an upload's body
    /// (PLATFORM-073). The route's unit of work, which [`Authorize::open`] opens afterwards,
    /// authorizes the caller again, so the operation still runs under the membership it was
    /// authorized with (PLATFORM-009).
    pub async fn check(&self) -> Result<(), PlatformError> {
        let (uow, workspace_id, decision) = self.decide().await?;
        drop(uow);
        match decision {
            Decision::Allowed(_) => Ok(()),
            refused => Err(self.refuse(workspace_id, refused).await),
        }
    }
    // end part: files

    /// Opens a unit of work and asks the policy whether the caller holds `P` in the workspace
    /// of the path. A workspace ID that isn't a UUID names no workspace, which answers 404
    /// unrecorded (ACCESS-003).
    async fn decide(&self) -> Result<(UnitOfWork, WorkspaceId, Decision), PlatformError> {
        let Some(workspace_id) = self.workspace_id else {
            return Err(RequestError::NotFound.into());
        };
        let mut uow = self.authorizer.database.begin(&self.client).await?;
        let decision = self
            .authorizer
            .policy
            .authorize(&mut uow, self.principal.user_id(), workspace_id, P::NAME)
            .await?;
        Ok((uow, workspace_id, decision))
    }

    /// Records and counts the refusal of `decision`, which isn't [`Decision::Allowed`], and
    /// answers it: 404 for a caller who isn't a member, 403 for a member without `P`.
    async fn refuse(&self, workspace_id: WorkspaceId, decision: Decision) -> PlatformError {
        let member = decision != Decision::NotMember;
        let refusal = Refusal {
            user_id: self.principal.user_id(),
            workspace_id,
            permission: P::NAME,
            member,
            client: self.client.clone(),
        };
        let (reason, answer) = if member {
            (Reason::Permission, RequestError::PermissionDenied)
        } else {
            (Reason::NotMember, RequestError::NotFound)
        };
        match report(&*self.authorizer.sink, &refusal, reason).await {
            Ok(()) => answer.into(),
            Err(error) => error.into(),
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

    /// The scope of a job this member's operation enqueues: the workspace, and the member
    /// (PLATFORM-013).
    pub fn job_scope(&self) -> JobScope {
        JobScope::proven(Some(self.principal.user_id()), Some(self.workspace_id))
    }
}

/// Refuses an operation that found its member may not act on another member of the same or a
/// higher rank (SEC-09): rolls `uow` back, records the refusal, and answers 403
/// `request.permission_denied` (PLATFORM-010).
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
/// goes unrecorded while a log is left to hold it (DATA-22, AUDIT-004).
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
