//! What authorizes a superadmin's routes: the proof [`Superadmin`] that the caller is one of the
//! deployment's superadmins, which the [`SuperadminPolicy`] that `superadmins` implements
//! decides inside the request's unit of work, and the report of every refusal to the
//! `SecurityEventSink` (D62, SUPERADMINS-004).

use axum::{
    extract::FromRequestParts,
    http::request::Parts,
    response::{IntoResponse, Response},
};

use super::{Authorizer, REFUSALS, Reason};
use crate::{
    authn::{AuditScope, BoxFuture, Log, Principal},
    db::UnitOfWork,
    errors::{Internal, PlatformError, Problem, RequestError},
    types::{Client, UserId, WorkspaceId},
};

/// What [`SuperadminPolicy::admits`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The caller is no superadmin.
    Refused,
    /// The caller is a superadmin whose account has no second factor, which every superadmin
    /// must have (D63).
    WithoutSecondFactor,
    /// The caller is a superadmin with a second factor.
    Admitted,
}

/// Decides whether a caller is a superadmin, which `superadmins` implements for `platform`
/// (ARCH-13).
pub trait SuperadminPolicy: Send + Sync {
    /// Whether `user` is a superadmin, and, when they are, whether their account has a second
    /// factor, read inside `uow`, the request's unit of work (SUPERADMINS-004, SUPERADMINS-005).
    fn admits<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        user: UserId,
    ) -> BoxFuture<'a, Result<Admission, Internal>>;
}

/// The extractor of a superadmin's route: the caller, which proves nothing until
/// [`AuthorizeSuperadmin::open`]. The handler calls it once its input is parsed, so the unit of
/// work opens last (architecture §6).
pub struct AuthorizeSuperadmin {
    principal: Principal,
    client: Client,
    authorizer: Authorizer,
}

impl<S: Send + Sync> FromRequestParts<S> for AuthorizeSuperadmin {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Some(authorizer) = parts.extensions.get::<Authorizer>().cloned() else {
            return Err(Problem(Internal::new("the request carries no authorizer")).into_response());
        };
        let principal = Principal::from_request_parts(parts, state).await?;
        let client = Client::from_request_parts(parts, state).await?;
        Ok(Self {
            principal,
            client,
            authorizer,
        })
    }
}

impl AuthorizeSuperadmin {
    /// Opens the request's unit of work and asks the policy inside it whether the caller is a
    /// superadmin: the open unit of work with the proof, or 403 `request.permission_denied` for
    /// a caller who is none, recorded and counted first (SUPERADMINS-004), or 403
    /// `request.second_factor_setup_required` for a superadmin whose account has no second
    /// factor (SUPERADMINS-005).
    pub async fn open(self) -> Result<(UnitOfWork, Superadmin), PlatformError> {
        let mut uow = self.authorizer.database.begin(&self.client).await?;
        let user = self.principal.user_id();
        match self.authorizer.superadmins.admits(&mut uow, user).await? {
            Admission::Admitted => Ok((
                uow,
                Superadmin {
                    principal: self.principal,
                },
            )),
            Admission::WithoutSecondFactor => Err(RequestError::SecondFactorSetupRequired.into()),
            Admission::Refused => {
                drop(uow);
                REFUSALS.add(1, [Reason::Permission.label().into()]);
                self.authorizer
                    .sink
                    .refused_superadmin(self.principal.audit_scope(), &self.client)
                    .await?;
                Err(RequestError::PermissionDenied.into())
            }
        }
    }
}

/// The proof that the caller is a superadmin with a second factor, built only by
/// [`AuthorizeSuperadmin::open`] (D62). The operation runs in the unit of work it was proven in,
/// and a function that exists only for a superadmin's routes takes it, such as one that reads
/// across workspaces, or that first reaches the workspace that a path names (DATA-16).
pub struct Superadmin {
    principal: Principal,
}

impl Superadmin {
    /// The caller, whose own log holds what they change as a superadmin, under the scope of
    /// [`Principal::audit_scope`], and whose session an operation that needs its proof reads
    /// (WORKSPACES-030).
    pub fn principal(&self) -> &Principal {
        &self.principal
    }

    /// The scope of the entry that records this superadmin creating `workspace`, whose ID the
    /// database just gave it, and which they own: the workspace's log, with them as the actor,
    /// before any `Authorized<P>` can name it (WORKSPACES-031). Only a superadmin's creation of
    /// a workspace calls it; clippy bans it elsewhere.
    pub fn owner_scope(&self, workspace: WorkspaceId) -> AuditScope {
        AuditScope::proven(Log::Workspace(workspace), self.principal.user_id())
    }
}
