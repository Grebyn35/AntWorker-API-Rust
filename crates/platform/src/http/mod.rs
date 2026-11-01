//! The request pipeline: the route builders, the OpenAPI document they produce, and the
//! middleware stack ([architecture §6](../../../../docs/architecture.md#6-request-lifecycle)).

mod body;
mod browsers;
mod client;
mod docs;
mod limits;
pub(crate) mod query;
mod stack;

use std::sync::Arc;

use axum::{Router, routing::get};
use utoipa::openapi::{Info, OpenApi};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

pub use body::{FieldErrors, Idempotent, Json};
pub use browsers::{Cookies, Origins, expired_cookie, session_cookie};
pub use query::Query;

use crate::{
    authn::{Authenticator, SessionVerifier},
    authz::{Authorizer, Permission},
    clock::SharedClock,
    config::RateLimitConfig,
    errors::{ErrorCode, Problem, RequestError},
    lifecycle::{self, Readiness},
    rate_limit::{Bucket, RateLimiter},
    telemetry::Service,
};
use ipnet::IpNet;

/// Who may call a route (API-14).
#[derive(Clone, Copy, Debug)]
enum Access {
    /// Anyone, without authentication.
    Public,
    /// A caller whom a `Principal` proves.
    User,
    /// A member of the workspace in the path whom `Authorized<P>` proves, for the permission
    /// of this name.
    Workspace(&'static str),
}

impl Access {
    /// The value of the operation's `x-access` extension.
    fn name(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::User => "user",
            Self::Workspace(_) => "workspace",
        }
    }
}

/// A module's routes, each registered with its documentation. Every route is added through a
/// builder that states its access level (API-14), and registers the route at the paths its
/// documentation names (API-17).
pub struct Routes<S = ()> {
    router: OpenApiRouter<S>,
}

impl<S> Default for Routes<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self {
            router: OpenApiRouter::new(),
        }
    }
}

impl<S> Routes<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// No routes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a route that anyone may call, without authentication, which can answer `errors`
    /// besides the pipeline's own (ERR-13).
    pub fn public(self, route: UtoipaMethodRouter<S>, errors: &[&dyn ErrorCode]) -> Self {
        self.add(Access::Public, route, errors)
    }

    /// Adds a route that a signed-in user calls, whose handler extracts the caller's `Principal`,
    /// and which can answer `errors` besides the pipeline's own (ERR-13).
    pub fn user(self, route: UtoipaMethodRouter<S>, errors: &[&dyn ErrorCode]) -> Self {
        self.add(Access::User, route, errors)
    }

    /// Adds a route that a member of the workspace in the path calls, whose handler extracts
    /// `Authorize<P>`, and which can answer `errors` besides the pipeline's own (ERR-13).
    pub fn workspace<P: Permission>(
        self,
        route: UtoipaMethodRouter<S>,
        errors: &[&dyn ErrorCode],
    ) -> Self {
        self.add(Access::Workspace(P::NAME), route, errors)
    }

    /// The routes, with the state their handlers extract.
    pub fn with_state<S2>(self, state: S) -> Routes<S2> {
        Routes {
            router: self.router.with_state(state),
        }
    }

    /// The routes of both.
    #[must_use]
    #[expect(
        clippy::disallowed_methods,
        reason = "the builders are the one place that registers routes (API-14, API-17)"
    )]
    pub fn merge(self, other: Self) -> Self {
        Self {
            router: self.router.merge(other.router),
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the builders are the one place that registers routes (API-14, API-17)"
    )]
    fn add(
        self,
        access: Access,
        (mut schemas, mut paths, route): UtoipaMethodRouter<S>,
        errors: &[&dyn ErrorCode],
    ) -> Self {
        let queries = docs::document(&mut paths, access, errors);
        schemas.extend(docs::problem_schemas());
        let route = stack::route_group(route, queries);
        Self {
            router: self.router.routes((schemas, paths, route)),
        }
    }
}

impl<S> Routes<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// The OpenAPI document of the routes, for the service `service`, with the problems the
    /// router answers when no route matches. It needs no state, so a module's routes document
    /// themselves before the composition root gives them theirs.
    pub fn openapi(&self, service: Service) -> OpenApi {
        let mut document = self.router.get_openapi().clone();
        document.info = Info::new(service.name, service.version);
        docs::fallbacks(&mut document);
        document
    }
}

/// What the request pipeline takes from the deployment.
pub struct Pipeline {
    /// The clock the request log measures durations with.
    pub clock: SharedClock,
    /// The most requests served at once, health probes aside (OPS-07).
    pub max_concurrent_requests: usize,
    /// What the readiness probe reports (OPS-05).
    pub readiness: Readiness,
    /// The proxies whose `X-Forwarded-For` names the client (PLATFORM-047).
    pub trusted_proxies: Vec<IpNet>,
    /// The origins of web clients (PLATFORM-052).
    pub origins: Origins,
    /// The limiter of every rate limit (PLATFORM-048).
    pub limiter: RateLimiter,
    /// The IP and user limits.
    pub rate_limits: RateLimitConfig,
    /// The verifier of access tokens, which `auth` implements (ARCH-13).
    pub verifier: Arc<dyn SessionVerifier>,
    /// What authorizes workspace routes, with the policy `access` implements (ARCH-13).
    pub authorizer: Authorizer,
}

/// The bucket of each client IP's requests (PLATFORM-048).
const IP_BUCKET: &str = "ip";

/// The bucket of each user's requests (PLATFORM-048).
const USER_BUCKET: &str = "user";

/// The application: every module's routes inside the middleware stack, the fallbacks, and the
/// health probes, answered before the stack (OPS-05). The security headers wrap them all
/// (SEC-11).
#[expect(
    clippy::disallowed_methods,
    reason = "the builders are the one place that registers routes (API-14, API-17)"
)]
pub fn app(routes: Routes, pipeline: Pipeline) -> Router {
    let (api, _) = routes.router.split_for_parts();
    let api = api
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed);
    let limits = pipeline.rate_limits;
    let api = stack::request(
        api,
        stack::Stack {
            clock: pipeline.clock,
            max_concurrent: pipeline.max_concurrent_requests,
            trusted_proxies: Arc::new(pipeline.trusted_proxies),
            origins: pipeline.origins,
            ip_limit: Arc::new(limits::IpLimit {
                limiter: pipeline.limiter,
                bucket: Bucket {
                    name: IP_BUCKET,
                    limit: limits.ip_requests,
                    window: limits.window,
                },
            }),
            authenticator: Authenticator {
                verifier: pipeline.verifier,
                user_bucket: Bucket {
                    name: USER_BUCKET,
                    limit: limits.user_requests,
                    window: limits.window,
                },
            },
            authorizer: pipeline.authorizer,
        },
    );
    let app = Router::new()
        .route("/health/live", get(lifecycle::live))
        .route("/health/ready", get(lifecycle::ready))
        .with_state(pipeline.readiness)
        .merge(api);
    stack::security_headers(app)
}

async fn not_found() -> Problem<RequestError> {
    Problem(RequestError::NotFound)
}

async fn method_not_allowed() -> Problem<RequestError> {
    Problem(RequestError::MethodNotAllowed)
}
