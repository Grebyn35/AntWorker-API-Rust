//! The layers of the request pipeline, in the order of
//! [architecture §6](../../../../docs/architecture.md#6-request-lifecycle).

use std::{any::Any, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    error_handling::HandleErrorLayer,
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::MethodRouter,
};
use http_body_util::Limited;
use opentelemetry::propagation::{Extractor, TextMapPropagator};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use tower::{
    BoxError, ServiceBuilder, limit::GlobalConcurrencyLimitLayer, load_shed::LoadShedLayer,
    timeout::TimeoutLayer,
};
use tower_http::{catch_panic::CatchPanicLayer, set_header::SetResponseHeaderLayer};
use tracing::{Instrument, field::Empty};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::docs::QueryNames;
use crate::{
    clock::SharedClock,
    errors::{ErrorReport, Internal, Problem, RequestError},
    telemetry::{self, names},
};

/// How long a request of the `public`, `user` and `workspace` groups may run: twice the five
/// seconds an operation waits for a pooled connection (OPS-07).
const GROUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest body a request of those groups may carry (API-08).
const GROUP_BODY_LIMIT: usize = 64 * 1024;

/// What a client should wait before it retries a request that load shedding refused.
const RETRY_AFTER_SECONDS: &str = "1";

/// The security headers on every response, health probes included (SEC-11).
pub(super) fn security_headers(router: Router) -> Router {
    router.layer(
        ServiceBuilder::new()
            .layer(SetResponseHeaderLayer::overriding(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ))
            .layer(SetResponseHeaderLayer::overriding(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            )),
    )
}

/// The stack every request below the security headers passes, health probes aside: trace
/// context, the request log, panic catching, and the concurrency limit.
pub(super) fn request(router: Router, clock: SharedClock, max_concurrent: usize) -> Router {
    router.layer(
        ServiceBuilder::new()
            .layer(middleware::from_fn(trace))
            .layer(middleware::from_fn_with_state(clock, log))
            .layer(CatchPanicLayer::custom(panicked))
            .layer(HandleErrorLayer::new(overloaded))
            .layer(LoadShedLayer::new())
            .layer(GlobalConcurrencyLimitLayer::new(max_concurrent)),
    )
}

/// The layers of a route group: its timeout and body limit, and the check of its query
/// parameters against its documentation.
pub(super) fn route_group<S>(route: MethodRouter<S>, queries: QueryNames) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    route.layer(
        ServiceBuilder::new()
            .layer(HandleErrorLayer::new(timed_out))
            .layer(TimeoutLayer::new(GROUP_TIMEOUT))
            .layer(middleware::from_fn(body_limit))
            .layer(middleware::from_fn_with_state(Arc::new(queries), query)),
    )
}

/// Opens the request's server span, as the child of a valid inbound `traceparent`, and writes
/// the span's trace context into `server-timing` (PLATFORM-022, API-16).
async fn trace(request: Request, next: Next) -> Response {
    let parent = TraceContextPropagator::new().extract(&Headers(request.headers()));
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|route| route.as_str().to_owned());
    let name = route
        .as_ref()
        .map_or_else(|| method.to_string(), |route| format!("{method} {route}"));
    let span = tracing::info_span!(
        "request",
        { names::OTEL_NAME } = name,
        { names::OTEL_KIND } = "server",
        { names::OTEL_STATUS_CODE } = Empty,
        { names::HTTP_REQUEST_METHOD } = %method,
        { names::HTTP_ROUTE } = route,
        { names::HTTP_RESPONSE_STATUS_CODE } = Empty,
    );
    // An inbound context that isn't valid leaves the span the root of a new trace.
    span.set_parent(parent).ok();
    let mut response = next.run(request).instrument(span.clone()).await;
    let status = response.status();
    span.record(names::HTTP_RESPONSE_STATUS_CODE, status.as_u16());
    if status.is_server_error() {
        span.record(names::OTEL_STATUS_CODE, "error");
    }
    if let Some(value) = telemetry::traceparent(&span)
        .and_then(|traceparent| HeaderValue::try_from(format!("trace;desc={traceparent}")).ok())
    {
        response
            .headers_mut()
            .insert(HeaderName::from_static("server-timing"), value);
    }
    response
}

/// Writes one `INFO` line when the request completes, and before it the `ERROR` line of a 5xx
/// with its cause chain (PLATFORM-034).
async fn log(State(clock): State<SharedClock>, request: Request, next: Next) -> Response {
    let start = clock.now();
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|route| route.as_str().to_owned());
    let mut response = next.run(request).await;
    let status = response.status();
    if status.is_server_error() {
        let report = response.extensions_mut().remove::<ErrorReport>();
        tracing::error!(
            { names::ERROR_TYPE } = report.as_ref().map(|report| report.code),
            { names::EXCEPTION_MESSAGE } = report.as_ref().map(|report| report.chain.as_str()),
            "request failed"
        );
    }
    tracing::info!(
        { names::HTTP_REQUEST_METHOD } = %method,
        { names::HTTP_ROUTE } = route,
        { names::HTTP_RESPONSE_STATUS_CODE } = status.as_u16(),
        { names::REQUEST_DURATION } = clock.now().duration_since(start).as_secs_f64(),
        "request completed"
    );
    response
}

/// Answers a panic with the 500 problem. The panic hook has already logged it (ERR-12).
fn panicked(_payload: Box<dyn Any + Send + 'static>) -> Response {
    Problem(RequestError::Panicked).into_response()
}

/// Answers a request that load shedding refused with the 503 problem and `Retry-After`
/// (PLATFORM-020).
async fn overloaded(error: BoxError) -> Response {
    if error.is::<tower::load_shed::error::Overloaded>() {
        let mut response = Problem(RequestError::Overloaded).into_response();
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_static(RETRY_AFTER_SECONDS),
        );
        response
    } else {
        Problem(Internal::new(error)).into_response()
    }
}

/// Answers a request past its route group's timeout with the 503 problem (PLATFORM-031).
async fn timed_out(error: BoxError) -> Response {
    if error.is::<tower::timeout::error::Elapsed>() {
        Problem(RequestError::TimedOut).into_response()
    } else {
        Problem(Internal::new(error)).into_response()
    }
}

/// Refuses a body whose declared length is over the route group's limit before reading it, and
/// limits what an extractor reads of any other body (PLATFORM-036).
async fn body_limit(request: Request, next: Next) -> Response {
    let declared = request.headers().get(header::CONTENT_LENGTH).map(|length| {
        length
            .to_str()
            .ok()
            .and_then(|length| length.parse::<usize>().ok())
    });
    // A length that isn't a number Rust can hold is over any limit.
    if declared.is_some_and(|length| length.is_none_or(|length| length > GROUP_BODY_LIMIT)) {
        return Problem(RequestError::PayloadTooLarge).into_response();
    }
    let request = request.map(|body| Body::new(Limited::new(body, GROUP_BODY_LIMIT)));
    next.run(request).await
}

/// Refuses query parameters that the route's documentation doesn't list (PLATFORM-035). A
/// method the route doesn't document passes, so the router answers it with 405.
async fn query(State(queries): State<Arc<QueryNames>>, request: Request, next: Next) -> Response {
    if let (Some(allowed), Some(query)) = (queries.get(request.method()), request.uri().query()) {
        let mut unknown: Vec<String> = url::form_urlencoded::parse(query.as_bytes())
            .map(|(name, _)| name.into_owned())
            .filter(|name| !allowed.contains(name))
            .collect();
        unknown.sort();
        unknown.dedup();
        if !unknown.is_empty() {
            return Problem(RequestError::InvalidQuery(unknown)).into_response();
        }
    }
    next.run(request).await
}

/// The request's headers, as the trace context propagator reads them.
struct Headers<'a>(&'a HeaderMap);

impl Extractor for Headers<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(HeaderName::as_str).collect()
    }
}
