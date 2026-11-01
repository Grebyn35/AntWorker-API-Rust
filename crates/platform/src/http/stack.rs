//! The layers of the request pipeline, in the order of
//! [architecture §6](../../../../docs/architecture.md#6-request-lifecycle).

use std::{any::Any, sync::Arc, time::Duration};

use axum::{
    Extension, Router,
    body::Body,
    error_handling::HandleErrorLayer,
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::MethodRouter,
};
use http_body_util::Limited;
use opentelemetry::propagation::{Extractor, TextMapPropagator};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_semantic_conventions::metric as otel;
use tower::{
    BoxError, ServiceBuilder, limit::GlobalConcurrencyLimitLayer, load_shed::LoadShedLayer,
    timeout::TimeoutLayer,
};
use tower_http::{catch_panic::CatchPanicLayer, set_header::SetResponseHeaderLayer};
use tracing::{Instrument, Span, field::Empty};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::{
    browsers::{Origins, cors, cross_site},
    client::{TrustedProxies, client},
    docs::QueryNames,
    limits::{IpLimit, ip_limit},
};
use crate::{
    authn::Authenticator,
    clock::SharedClock,
    errors::{self, ErrorReport, Internal, Problem, RequestError},
    telemetry::{
        self,
        metrics::Histogram,
        names::{self, convention},
    },
};

/// How long a request of the `public`, `user` and `workspace` groups may run: twice the five
/// seconds an operation waits for a pooled connection (OPS-07).
const GROUP_TIMEOUT: Duration = Duration::from_secs(10);

/// The largest body a request of those groups may carry (API-08).
const GROUP_BODY_LIMIT: usize = 64 * 1024;

/// What a client should wait before it retries a request that load shedding refused.
const RETRY_AFTER_SECONDS: &str = "1";

/// The method of a request whose method HTTP's specifications don't define, and the error type
/// of a 5xx whose error the request log doesn't know (PLATFORM-040).
const OTHER: &str = "_OTHER";

static REQUEST_DURATION: Histogram<4> = Histogram::new(
    convention(
        "http.server.request.duration",
        otel::HTTP_SERVER_REQUEST_DURATION,
    ),
    "s",
    "How long each request took, from the request log's start to its response, health probes \
     aside.",
    [
        names::HTTP_REQUEST_METHOD,
        names::HTTP_ROUTE,
        names::HTTP_RESPONSE_STATUS_CODE,
        names::ERROR_TYPE,
    ],
    // The boundaries the conventions advise for this metric.
    &[
        0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
    ],
);

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

/// What the stack below the security headers takes from the pipeline.
pub(super) struct Stack {
    pub(super) clock: SharedClock,
    pub(super) max_concurrent: usize,
    pub(super) trusted_proxies: TrustedProxies,
    pub(super) origins: Origins,
    pub(super) ip_limit: Arc<IpLimit>,
    pub(super) authenticator: Authenticator,
}

/// The stack every request below the security headers passes, health probes aside, in the order
/// of architecture §6: trace context, the request log, panic catching, the client, CORS, the
/// concurrency limit, the IP limit and the cross-site check.
pub(super) fn request(router: Router, stack: Stack) -> Router {
    router.layer(
        ServiceBuilder::new()
            .layer(middleware::from_fn(trace))
            .layer(middleware::from_fn_with_state(stack.clock, log))
            .layer(CatchPanicLayer::custom(panicked))
            .layer(middleware::from_fn_with_state(
                stack.trusted_proxies,
                client,
            ))
            .layer(middleware::from_fn_with_state(stack.origins.clone(), cors))
            .layer(HandleErrorLayer::new(overloaded))
            .layer(LoadShedLayer::new())
            .layer(GlobalConcurrencyLimitLayer::new(stack.max_concurrent))
            .layer(middleware::from_fn_with_state(stack.ip_limit, ip_limit))
            .layer(middleware::from_fn_with_state(stack.origins, cross_site))
            .layer(Extension(stack.authenticator)),
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
    let method = method_name(request.method());
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|route| route.as_str().to_owned());
    // The conventions name a span of an unknown method after the protocol instead.
    let shown = if method == OTHER { "HTTP" } else { method };
    let name = route
        .as_ref()
        .map_or_else(|| shown.to_owned(), |route| format!("{shown} {route}"));
    let span = tracing::info_span!(
        "request",
        { names::OTEL_NAME } = name,
        { names::OTEL_KIND } = "server",
        { names::OTEL_STATUS_CODE } = Empty,
        { names::HTTP_REQUEST_METHOD } = method,
        { names::HTTP_ROUTE } = route,
        { names::HTTP_RESPONSE_STATUS_CODE } = Empty,
        { names::ERROR_TYPE } = Empty,
    );
    // An inbound context that isn't valid leaves the span the root of a new trace. Setting the
    // parent fails only if the span isn't one the tracer follows, and then the request goes on
    // without the inbound trace.
    if let Err(error) = span.set_parent(parent) {
        errors::ignore(&error, "trace_parent_not_set");
    }
    let mut response = next.run(request).instrument(span.clone()).await;
    let status = response.status();
    // The bridge to OpenTelemetry records an unsigned value as text, and the conventions define
    // the status as a number, which the collector's sampling compares (OBS-08).
    span.record(names::HTTP_RESPONSE_STATUS_CODE, i64::from(status.as_u16()));
    if status.is_server_error() {
        span.record(names::OTEL_STATUS_CODE, "error");
    }
    if let Some(traceparent) = telemetry::traceparent(&span) {
        // A traceparent holds only hexadecimal digits and dashes, which a header value accepts.
        match HeaderValue::try_from(format!("trace;desc={traceparent}")) {
            Ok(value) => {
                response
                    .headers_mut()
                    .insert(HeaderName::from_static("server-timing"), value);
            }
            Err(error) => errors::ignore(&error, "server_timing_not_set"),
        }
    }
    response
}

/// Marks the response to a panic, whose `ERROR` line the panic hook has written, so the error is
/// logged once (ERR-10, ERR-12).
#[derive(Clone, Copy)]
struct PanicLogged;

/// Writes one `INFO` line when the request completes, and before it the `ERROR` line of a 5xx
/// with its cause chain, unless the 5xx answers a panic (PLATFORM-034). It records the request's
/// duration, and gives a 5xx's error code to the metric and the request's span as `error.type`
/// (PLATFORM-040).
async fn log(State(clock): State<SharedClock>, request: Request, next: Next) -> Response {
    let start = clock.now();
    let method = method_name(request.method());
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|route| route.as_str().to_owned());
    let mut response = next.run(request).await;
    let status = response.status();
    let panicked = response.extensions_mut().remove::<PanicLogged>().is_some();
    let mut error_type = None;
    if status.is_server_error() {
        let report = response.extensions_mut().remove::<ErrorReport>();
        let code = report.as_ref().map_or(OTHER, |report| report.code);
        Span::current().record(names::ERROR_TYPE, code);
        error_type = Some(code);
        if !panicked {
            tracing::error!(
                { names::ERROR_TYPE } = report.as_ref().map(|report| report.code),
                { names::EXCEPTION_MESSAGE } = report.as_ref().map(|report| report.chain.as_str()),
                "request failed"
            );
        }
    }
    let duration = clock.now().duration_since(start).as_secs_f64();
    tracing::info!(
        { names::HTTP_REQUEST_METHOD } = method,
        { names::HTTP_ROUTE } = route,
        { names::HTTP_RESPONSE_STATUS_CODE } = status.as_u16(),
        { names::REQUEST_DURATION } = duration,
        "request completed"
    );
    REQUEST_DURATION.observe(
        duration,
        [
            method.into(),
            route.into(),
            i64::from(status.as_u16()).into(),
            error_type.into(),
        ],
    );
    response
}

/// The method as OpenTelemetry's conventions give it: one of RFC 9110's methods, `PATCH` or
/// `QUERY`, or `_OTHER` for any other, so a client can't add values without bound.
fn method_name(method: &Method) -> &'static str {
    match method.as_str() {
        "CONNECT" => "CONNECT",
        "DELETE" => "DELETE",
        "GET" => "GET",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "PATCH" => "PATCH",
        "POST" => "POST",
        "PUT" => "PUT",
        "QUERY" => "QUERY",
        "TRACE" => "TRACE",
        _ => OTHER,
    }
}

/// Answers a panic with the 500 problem. The panic hook has already logged it, with its
/// message and location (ERR-12).
fn panicked(_payload: Box<dyn Any + Send + 'static>) -> Response {
    let mut response = Problem(RequestError::Panicked).into_response();
    response.extensions_mut().insert(PanicLogged);
    response
}

/// Answers a request that load shedding refused with the 503 problem and `Retry-After`
/// (PLATFORM-020).
async fn overloaded(error: BoxError) -> Response {
    if error.is::<tower::load_shed::error::Overloaded>() {
        retry_later(RequestError::Overloaded)
    } else {
        Problem(Internal::new(error)).into_response()
    }
}

/// Answers a request past its route group's timeout with the 503 problem and `Retry-After`
/// (PLATFORM-031).
async fn timed_out(error: BoxError) -> Response {
    if error.is::<tower::timeout::error::Elapsed>() {
        retry_later(RequestError::TimedOut)
    } else {
        Problem(Internal::new(error)).into_response()
    }
}

/// The 503 problem of `error`, with the `Retry-After` the document lists for it.
fn retry_later(error: RequestError) -> Response {
    let mut response = Problem(error).into_response();
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from_static(RETRY_AFTER_SECONDS),
    );
    response
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

/// Refuses query parameters that the route's documentation doesn't list (PLATFORM-035). `HEAD`
/// takes the parameters of the `GET` that serves it, and a method the route doesn't serve
/// passes, so the router answers it with 405.
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
