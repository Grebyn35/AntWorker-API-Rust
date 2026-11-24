//! What the request log writes once a request ends: its completion line, and its duration in
//! `http.server.request.duration`, of a request that got its response and of one that ended
//! before any (PLATFORM-044, PLATFORM-067).

use std::time::Instant;

use axum::http::StatusCode;
use opentelemetry_semantic_conventions::metric as otel;
use tracing::Span;

use crate::{
    clock::SharedClock,
    telemetry::{
        metrics::Histogram,
        names::{self, convention},
    },
};

/// The `error.type` of a request that ended before any response: its client closed the
/// connection, or the process ended while it ran, after the drain's deadline. The server can't
/// tell which.
pub(super) const UNANSWERED: &str = "ended_before_response";

static REQUEST_DURATION: Histogram<4> = Histogram::new(
    convention(
        "http.server.request.duration",
        otel::HTTP_SERVER_REQUEST_DURATION,
    ),
    "s",
    "How long each request took, from the request log's start to its response, or to its end \
     before any, health probes aside.",
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

/// A request that the request log follows from its start. [`Completion::write`] writes its line
/// and records its duration once it has its response. A request whose work the server drops
/// before then gets them as it is dropped, inside the request's span, with [`UNANSWERED`] as
/// `error.type` and no status, and the span gets that `error.type` too, its status unset: no
/// server error ended the request.
pub(super) struct Completion {
    clock: SharedClock,
    start: Instant,
    method: &'static str,
    route: Option<String>,
    written: bool,
}

impl Completion {
    /// Starts following a request of `method`, on `route` if it matched one.
    pub(super) fn start(clock: SharedClock, method: &'static str, route: Option<String>) -> Self {
        Self {
            start: clock.now(),
            clock,
            method,
            route,
            written: false,
        }
    }

    /// Writes the line, with the response's status, and records the duration, with the status
    /// and `error_type`, a 5xx's error code, or `_OTHER` for one without (PLATFORM-067).
    pub(super) fn write(mut self, status: StatusCode, error_type: Option<&'static str>) {
        self.end(Some(status.as_u16()), error_type);
    }

    fn end(&mut self, status: Option<u16>, error_type: Option<&'static str>) {
        self.written = true;
        let duration = self.clock.now().duration_since(self.start).as_secs_f64();
        tracing::info!(
            { names::HTTP_REQUEST_METHOD } = self.method,
            { names::HTTP_ROUTE } = self.route,
            { names::HTTP_RESPONSE_STATUS_CODE } = status,
            // A response's status says how its request ended, and a 5xx's `ERROR` line names
            // its error.
            { names::ERROR_TYPE } = status.is_none().then_some(UNANSWERED),
            { names::REQUEST_DURATION } = duration,
            "request completed"
        );
        REQUEST_DURATION.observe(
            duration,
            [
                self.method.into(),
                self.route.take().into(),
                status.map(i64::from).into(),
                error_type.into(),
            ],
        );
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        if !self.written {
            Span::current().record(names::ERROR_TYPE, UNANSWERED);
            self.end(None, Some(UNANSWERED));
        }
    }
}
