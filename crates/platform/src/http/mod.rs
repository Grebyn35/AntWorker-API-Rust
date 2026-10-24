//! The request pipeline: the route builders, the OpenAPI document they produce, and the
//! middleware stack ([architecture §6](../../../../docs/architecture.md#6-request-lifecycle)).

mod docs;
mod stack;

use axum::{Router, routing::get};
use utoipa::openapi::{Info, OpenApi};
use utoipa_axum::router::{OpenApiRouter, UtoipaMethodRouter};

use crate::{
    clock::SharedClock,
    errors::{Problem, RequestError},
    lifecycle::{self, Readiness},
    telemetry::Service,
};

/// Who may call a route (API-14).
#[derive(Clone, Copy, Debug)]
enum Access {
    /// Anyone, without authentication.
    Public,
}

impl Access {
    /// The value of the operation's `x-access` extension.
    fn name(self) -> &'static str {
        match self {
            Self::Public => "public",
        }
    }
}

/// A module's routes, each registered with its documentation. Every route is added through a
/// builder that states its access level (API-14), and takes the output of
/// `utoipa_axum::routes!`, so a route serves the path and methods its `#[utoipa::path]`
/// documents, and `HEAD` wherever it serves `GET` (API-17).
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

    /// Adds a route that anyone may call, without authentication.
    pub fn public(self, route: UtoipaMethodRouter<S>) -> Self {
        self.add(Access::Public, route)
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
    fn add(self, access: Access, (mut schemas, mut paths, route): UtoipaMethodRouter<S>) -> Self {
        let queries = docs::document(&mut paths, access);
        schemas.push(docs::problem_schema());
        let route = stack::route_group(route, queries);
        Self {
            router: self.router.routes((schemas, paths, route)),
        }
    }
}

impl Routes {
    /// The OpenAPI document of the routes, for the service `service`, with the problems the
    /// router answers when no route matches.
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
}

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
    let api = stack::request(api, pipeline.clock, pipeline.max_concurrent_requests);
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
