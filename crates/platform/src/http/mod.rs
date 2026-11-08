//! The request pipeline: the route builders, the OpenAPI document they produce, and the
//! middleware stack ([architecture §6](../../../../docs/architecture.md#6-request-lifecycle)).

mod body;
mod browsers;
mod client;
mod docs;
// part: files
mod files;
// end part: files
mod limits;
mod precondition;
pub(crate) mod query;
mod reference;
mod resource;
// part: files
pub(crate) mod signed;
// end part: files
mod stack;
// part: files
mod upload;
// end part: files

use std::sync::Arc;

use axum::{
    Router,
    routing::{MethodRouter, get},
};
use utoipa::openapi::{Info, OpenApi};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

pub use body::{FieldErrors, Idempotent, Json, MergePatch};
pub use browsers::{Cookies, Origins, expired_cookie, session_cookie};
pub use precondition::IfMatch;
pub use query::Query;
pub use resource::{ResourceResponse, path_id};
#[cfg(test)]
pub(crate) use stack::GROUP_TIMEOUT;
// part: files
pub use files::{Download, FileBytes, disposition_name};
#[cfg(test)]
pub(crate) use stack::UPLOAD_TIMEOUT;
pub use upload::{Upload, Uploaded};
// end part: files

use crate::{
    authn::{Authenticator, SessionVerifier},
    authz::{Authorizer, Permission},
    clock::SharedClock,
    config::RateLimitConfig,
    errors::{ErrorCode, Problem, RequestError},
    events::Catalogued,
    lifecycle::{self, Readiness},
    rate_limit::{Bucket, RateLimiter},
    telemetry::Service,
};
use ipnet::IpNet;
// part: files
use crate::signed_urls::SignedUrls;
// end part: files

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

    // part: files
    /// Adds an upload route that a member of the workspace in the path calls, whose handler
    /// extracts `Authorize<P>` and reads the file's bytes through [`Upload`], and which can answer
    /// `errors` besides the pipeline's own (ERR-13). Its group has the upload timeout, and no body
    /// limit of its own (PLATFORM-074).
    pub fn workspace_upload<P: Permission>(
        self,
        route: UtoipaMethodRouter<S>,
        errors: &[&dyn ErrorCode],
    ) -> Self {
        self.add_with(
            Access::Workspace(P::NAME),
            route,
            errors,
            stack::upload_group,
        )
    }

    /// Adds an upload route that a signed-in user calls, whose handler extracts the caller's
    /// `Principal` and reads the file's bytes through [`Upload`], and which can answer `errors`
    /// besides the pipeline's own (ERR-13). Its group is a workspace upload's (PLATFORM-074).
    pub fn user_upload(self, route: UtoipaMethodRouter<S>, errors: &[&dyn ErrorCode]) -> Self {
        self.add_with(Access::User, route, errors, stack::upload_group)
    }

    /// Adds a download route that a member of the workspace in the path calls, as
    /// [`Routes::workspace`] does, whose `Authorize<P>` a signed URL may also authenticate: the
    /// route documents the URL's query parameters, and answers `errors` besides the pipeline's
    /// own (PLATFORM-080, ERR-13).
    pub fn workspace_download<P: Permission>(
        self,
        mut route: UtoipaMethodRouter<S>,
        errors: &[&dyn ErrorCode],
    ) -> Self {
        docs::download(&mut route.1, false);
        docs::signed(&mut route.1);
        self.add_with(
            Access::Workspace(P::NAME),
            route,
            errors,
            stack::download_group,
        )
    }

    /// Adds a download route that a signed-in user calls, as [`Routes::user`] does, whose
    /// `Principal` a signed URL may also prove: the route documents the URL's query parameters,
    /// and answers `errors` besides the pipeline's own (PLATFORM-080, ERR-13).
    pub fn user_download(
        self,
        mut route: UtoipaMethodRouter<S>,
        errors: &[&dyn ErrorCode],
    ) -> Self {
        docs::download(&mut route.1, false);
        docs::signed(&mut route.1);
        self.add_with(Access::User, route, errors, stack::download_group)
    }

    /// Adds a download route that anyone may call, without authentication, as [`Routes::public`]
    /// does, whose path is under [`PUBLIC_PREFIX`], and which can answer `errors` besides the
    /// pipeline's own: a public file's, which shared caches may keep (PLATFORM-081, ERR-13).
    pub fn public_download(
        self,
        mut route: UtoipaMethodRouter<S>,
        errors: &[&dyn ErrorCode],
    ) -> Self {
        docs::download(&mut route.1, true);
        self.add(Access::Public, route, errors)
    }
    // end part: files

    /// Registers `events`, which the module's routes publish or send, in the event catalog of
    /// the routes' document (REALTIME-008).
    #[must_use]
    pub fn events(mut self, events: &[&dyn Catalogued]) -> Self {
        let components = self
            .router
            .get_openapi_mut()
            .components
            .get_or_insert_default();
        for event in events {
            event.entry().document(components);
        }
        self
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

    /// Adds `route`, documented with its access level and `errors`, inside its route group's
    /// layers.
    fn add(self, access: Access, route: UtoipaMethodRouter<S>, errors: &[&dyn ErrorCode]) -> Self {
        self.add_with(access, route, errors, stack::route_group)
    }

    /// Adds `route`, documented with its access level and `errors`, inside the layers `group`
    /// gives it.
    #[expect(
        clippy::disallowed_methods,
        reason = "the builders are the one place that registers routes (API-14, API-17)"
    )]
    fn add_with(
        self,
        access: Access,
        (mut schemas, mut paths, route): UtoipaMethodRouter<S>,
        errors: &[&dyn ErrorCode],
        group: fn(MethodRouter<S>, docs::QueryNames) -> MethodRouter<S>,
    ) -> Self {
        let queries = docs::document(&mut paths, access, errors);
        schemas.extend(docs::problem_schemas());
        let route = group(route, queries);
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

/// The OpenAPI document of `parts`, the documents of the modules' routes, merged, with the event
/// catalog `Frame` built from every frame type they register, which each stream's media type names
/// (REALTIME-008). Building it again from a document it built, merged with more, builds the
/// catalog of all of them.
pub fn document(parts: impl IntoIterator<Item = OpenApi>) -> OpenApi {
    let mut parts = parts.into_iter();
    let mut document = parts.next().unwrap_or_default();
    for part in parts {
        document.merge(part);
    }
    docs::catalog(&mut document);
    document
}

/// What the request pipeline takes from the deployment.
pub struct Pipeline {
    /// The clock the request log measures durations with.
    pub clock: SharedClock,
    /// The most requests served at once, health probes aside (OPS-07).
    pub max_concurrent_requests: usize,
    /// What the readiness probe reports (OPS-05).
    pub readiness: Readiness,
    /// The proxies whose `X-Forwarded-For` names the client (PLATFORM-053).
    pub trusted_proxies: Vec<IpNet>,
    /// The origins of web clients (PLATFORM-058).
    pub origins: Origins,
    /// The limiter of every rate limit (PLATFORM-054).
    pub limiter: RateLimiter,
    /// The IP and user limits.
    pub rate_limits: RateLimitConfig,
    /// The verifier of access tokens, which `auth` implements (ARCH-13).
    pub verifier: Arc<dyn SessionVerifier>,
    // part: files
    /// The checker of downloads' signed URLs (PLATFORM-080).
    pub signed_urls: SignedUrls,
    // end part: files
    /// What authorizes workspace routes, with the policy `access` implements (ARCH-13).
    pub authorizer: Authorizer,
    /// The OpenAPI document, as JSON, which the API reference shows: only in development
    /// (PLATFORM-052).
    pub reference: Option<Arc<str>>,
}

/// The bucket of each client IP's requests (PLATFORM-054).
const IP_BUCKET: &str = "ip";

// part: files
/// The bucket of each client IP's requests under [`PUBLIC_PREFIX`] (PLATFORM-082).
const PUBLIC_BUCKET: &str = "public";

/// The prefix of the paths of public files' downloads, which anyone may read without signing in,
/// whose requests count in [`PUBLIC_BUCKET`] (API-02, PLATFORM-082).
pub const PUBLIC_PREFIX: &str = "/v1/public/";
// end part: files

/// The bucket of each user's requests (PLATFORM-054).
const USER_BUCKET: &str = "user";

/// The application: every module's routes inside the middleware stack, the fallbacks, the API
/// reference when the pipeline holds the document, in development (PLATFORM-052), and the health
/// probes, answered before the stack (OPS-05). The security headers wrap them all (SEC-12).
#[expect(
    clippy::disallowed_methods,
    reason = "the builders are the one place that registers routes (API-14, API-17)"
)]
pub fn app(routes: Routes, pipeline: Pipeline) -> Router {
    let (api, _) = routes.router.split_for_parts();
    let api = match pipeline.reference {
        Some(document) => reference::mount(api, document),
        None => api,
    };
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
                // part: files
                public_bucket: Bucket {
                    name: PUBLIC_BUCKET,
                    limit: limits.public_requests,
                    window: limits.window,
                },
                // end part: files
            }),
            authenticator: Authenticator {
                verifier: pipeline.verifier,
                // part: files
                signed: pipeline.signed_urls,
                // end part: files
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
