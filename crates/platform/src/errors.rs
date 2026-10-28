//! Errors shared by every module, and the one conversion of an error into a response (ERR-07).

use std::{error::Error, fmt};

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::telemetry::{self, metrics::Counter, names};

/// The kind of a failure. The category alone sets the status of the response (ERR-03).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// Malformed input, or a field that fails validation.
    Validation,
    /// Missing, invalid or expired credentials.
    Unauthenticated,
    /// Authenticated, but not allowed, within a resource the caller can see.
    Forbidden,
    /// The resource doesn't exist, or the caller may not know it exists.
    NotFound,
    /// The path exists but doesn't serve the method.
    MethodNotAllowed,
    /// The request conflicts with the current state.
    Conflict,
    /// An `If-Match` that doesn't match (API-13).
    PreconditionFailed,
    /// A body over its limit.
    PayloadTooLarge,
    /// Valid input that a rule refuses.
    BusinessRule,
    /// A rate limit was hit.
    RateLimited,
    /// A disabled feature, a dependency that's down, load shedding, or a request past its
    /// timeout.
    Unavailable,
    /// Anything else.
    Internal,
}

impl Category {
    /// The status of every response in this category.
    pub fn status(self) -> StatusCode {
        match self {
            Self::Validation => StatusCode::BAD_REQUEST,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::Conflict => StatusCode::CONFLICT,
            Self::PreconditionFailed => StatusCode::PRECONDITION_FAILED,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::BusinessRule => StatusCode::UNPROCESSABLE_ENTITY,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// An error a client can receive (ERR-02).
pub trait ErrorCode: Error {
    /// `<namespace>.<snake_case_name>`, such as `workspaces.last_owner`. Once released, a code
    /// never changes.
    fn code(&self) -> &'static str;

    /// A short summary, the same for every occurrence of the code.
    fn title(&self) -> &'static str;

    /// The category, which sets the status.
    fn category(&self) -> Category;

    /// A sentence about this occurrence, for humans (ERR-09).
    fn detail(&self) -> Option<String> {
        None
    }
}

/// The response that reports `E` as an RFC 9457 problem (API-07). A handler returns
/// `Result<T, Problem<E>>`, and `?` converts an `E` into its problem.
#[derive(Debug)]
pub struct Problem<E>(pub E);

impl<E: ErrorCode> From<E> for Problem<E> {
    fn from(error: E) -> Self {
        Self(error)
    }
}

impl<E: ErrorCode + 'static> IntoResponse for Problem<E> {
    fn into_response(self) -> Response {
        response(&self.0)
    }
}

/// The body of every error response, an RFC 9457 problem with the error's stable code and the
/// request's trace ID (API-07).
#[derive(Debug, Serialize, ToSchema)]
#[schema(as = Problem, description = "An RFC 9457 problem, with the error's stable code and the request's trace ID.")]
pub(crate) struct ProblemBody {
    /// `/problems/<code>`.
    #[serde(rename = "type")]
    kind: String,
    /// A short summary, the same for every occurrence of the code.
    title: &'static str,
    /// The response's status.
    status: u16,
    /// A sentence about this occurrence, for humans. Clients act on `code`, never on this.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    detail: Option<String>,
    /// The stable error code.
    code: &'static str,
    /// The trace ID of the request.
    trace_id: String,
}

/// The cause chain of a 5xx response, which the request log writes and removes (ERR-10).
#[derive(Clone, Debug)]
pub(crate) struct ErrorReport {
    pub(crate) code: &'static str,
    pub(crate) chain: String,
}

/// The one conversion of an error into a response (ERR-07). A 5xx response has no detail, an
/// internal error answers with the fixed code and title (ERR-08), and every 5xx carries its cause
/// chain for the request log.
fn response(error: &(dyn ErrorCode + 'static)) -> Response {
    let status = error.category().status();
    let detail = if status.is_server_error() {
        None
    } else {
        error.detail()
    };
    // Every internal error answers the same, so its response reveals nothing about it (ERR-08).
    let (code, title) = if error.category() == Category::Internal {
        (INTERNAL_CODE, INTERNAL_TITLE)
    } else {
        (error.code(), error.title())
    };
    let body = ProblemBody {
        kind: format!("/problems/{code}"),
        title,
        status: status.as_u16(),
        detail,
        code,
        trace_id: telemetry::trace_id(),
    };
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    if status.is_server_error() {
        response.extensions_mut().insert(ErrorReport {
            code: error.code(),
            chain: chain(error),
        });
    }
    response
}

/// The errors `platform` answers before any module runs, in the namespace `request` (ERR-02).
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    /// No route has the path.
    #[error("no route has the path")]
    NotFound,
    /// The path doesn't serve the method.
    #[error("the path doesn't serve the method")]
    MethodNotAllowed,
    /// The query holds parameters that the route's documentation doesn't list (API-09).
    #[error("the query holds parameters the route doesn't take")]
    InvalidQuery(Vec<String>),
    /// The body is larger than the route's limit (API-08).
    #[error("the body is larger than the route's limit")]
    PayloadTooLarge,
    /// The request ran past its timeout (OPS-07).
    #[error("the request ran past its timeout")]
    TimedOut,
    /// The instance is serving as many requests as it may (OPS-07).
    #[error("the instance is serving as many requests as it may")]
    Overloaded,
    /// No replica of the API can answer, such as while the reference deployment restores its
    /// database. The proxy in front of the replicas answers it, never the API itself (OPS-05,
    /// D53), so clients find it in the same catalog as the API's own codes (ERR-13).
    #[error("no replica of the API can answer")]
    Unavailable,
    /// The handler panicked (ERR-12).
    #[error("the handler panicked")]
    Panicked,
}

impl ErrorCode for RequestError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "request.not_found",
            Self::MethodNotAllowed => "request.method_not_allowed",
            Self::InvalidQuery(_) => "request.invalid_query",
            Self::PayloadTooLarge => "request.payload_too_large",
            Self::TimedOut => "request.timed_out",
            Self::Overloaded => "request.overloaded",
            Self::Unavailable => "request.unavailable",
            Self::Panicked => INTERNAL_CODE,
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::NotFound => "Not found",
            Self::MethodNotAllowed => "Method not allowed",
            Self::InvalidQuery(_) => "Invalid query",
            Self::PayloadTooLarge => "Body too large",
            Self::TimedOut => "Request timed out",
            Self::Overloaded => "Too many requests in progress",
            Self::Unavailable => "No replica can answer",
            Self::Panicked => INTERNAL_TITLE,
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::NotFound => Category::NotFound,
            Self::MethodNotAllowed => Category::MethodNotAllowed,
            Self::InvalidQuery(_) => Category::Validation,
            Self::PayloadTooLarge => Category::PayloadTooLarge,
            Self::TimedOut | Self::Overloaded | Self::Unavailable => Category::Unavailable,
            Self::Panicked => Category::Internal,
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::InvalidQuery(names) => Some(format!(
                "The route takes no query parameter named {}.",
                names.join(", ")
            )),
            _ => None,
        }
    }
}

impl RequestError {
    /// One value of every variant, so the OpenAPI document can list every code `platform`
    /// answers (ERR-13). A new variant is added here too.
    pub fn every() -> [Self; 8] {
        [
            Self::NotFound,
            Self::MethodNotAllowed,
            Self::InvalidQuery(Vec::new()),
            Self::PayloadTooLarge,
            Self::TimedOut,
            Self::Overloaded,
            Self::Unavailable,
            Self::Panicked,
        ]
    }
}

const INTERNAL_CODE: &str = "request.internal_error";
const INTERNAL_TITLE: &str = "Internal error";

/// A failure in infrastructure, such as the database. It keeps its cause for the logs, and
/// nothing about the cause reaches a client (ERR-04, ERR-08).
pub struct Internal {
    cause: Box<dyn Error + Send + Sync>,
}

impl Internal {
    /// An internal error caused by `cause`.
    pub fn new(cause: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        Self {
            cause: cause.into(),
        }
    }
}

impl From<sqlx::Error> for Internal {
    fn from(cause: sqlx::Error) -> Self {
        Self::new(cause)
    }
}

impl fmt::Display for Internal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("internal error")
    }
}

/// Shows the cause chain as `Display` text only. A cause's own `Debug` output can carry
/// personal data, such as the row a database constraint rejected (OBS-04).
impl fmt::Debug for Internal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Internal({})", chain(self.cause.as_ref()))
    }
}

impl Error for Internal {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

impl ErrorCode for Internal {
    fn code(&self) -> &'static str {
        INTERNAL_CODE
    }

    fn title(&self) -> &'static str {
        INTERNAL_TITLE
    }

    fn category(&self) -> Category {
        Category::Internal
    }
}

static ERRORS_IGNORED: Counter<1> = Counter::new(
    "platform.errors_ignored",
    "{error}",
    "Errors ignored on purpose, by the name of the failure (ERR-11).",
    [names::ERROR_TYPE],
);

/// Ignores `error` on purpose, writes one `WARN` line that says so, and counts it (ERR-11).
/// `reason` is a short `snake_case` name for the failure, such as `tracer_shutdown`, written as
/// `error.type` in the line and the count, and the line's `exception.message` is the error's
/// cause chain.
pub fn ignore(error: &(dyn Error + 'static), reason: &'static str) {
    tracing::warn!(
        { names::ERROR_TYPE } = reason,
        { names::EXCEPTION_MESSAGE } = chain(error),
        "error ignored"
    );
    ERRORS_IGNORED.add(1, [reason.into()]);
}

/// `error` and each of its causes, outermost first, joined with `: `.
pub fn chain(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("outer")]
    struct Outer(#[source] std::io::Error);

    #[test]
    fn err_10_a_chain_lists_every_cause_outermost_first() {
        let error = Outer(std::io::Error::other("inner"));
        assert_eq!(chain(&error), "outer: inner");
    }

    /// A cause whose `Debug` output holds personal data, as a database error's detail can.
    #[derive(thiserror::Error)]
    #[error("duplicate key value violates unique constraint")]
    struct Rejected;

    impl fmt::Debug for Rejected {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("Rejected { detail: \"Key (email)=(alice@example.com) already exists.\" }")
        }
    }

    #[test]
    fn obs_04_an_internal_error_never_prints_its_cause_through_debug() {
        let error = Internal::new(Rejected);
        let debug = format!("{error:?}");
        assert!(!debug.contains("alice@example.com"));
        assert_eq!(
            debug,
            "Internal(duplicate key value violates unique constraint)"
        );
    }

    #[test]
    fn err_03_each_category_has_its_status() {
        let statuses: Vec<u16> = [
            Category::Validation,
            Category::Unauthenticated,
            Category::Forbidden,
            Category::NotFound,
            Category::MethodNotAllowed,
            Category::Conflict,
            Category::PreconditionFailed,
            Category::PayloadTooLarge,
            Category::BusinessRule,
            Category::RateLimited,
            Category::Unavailable,
            Category::Internal,
        ]
        .into_iter()
        .map(|category| category.status().as_u16())
        .collect();
        assert_eq!(
            statuses,
            [400, 401, 403, 404, 405, 409, 412, 413, 422, 429, 503, 500]
        );
    }

    /// A module's failure in the category `Internal`, with a code and title of its own.
    #[derive(Debug, thiserror::Error)]
    #[error("the store is gone")]
    struct StoreGone;

    impl ErrorCode for StoreGone {
        fn code(&self) -> &'static str {
            "files.store_gone"
        }

        fn title(&self) -> &'static str {
            "Store gone"
        }

        fn category(&self) -> Category {
            Category::Internal
        }

        fn detail(&self) -> Option<String> {
            Some("the bucket was deleted".to_owned())
        }
    }

    #[test]
    fn err_08_every_internal_error_answers_the_fixed_code_and_title() {
        let response = Problem(StoreGone).into_response();
        let report = response.extensions().get::<ErrorReport>().unwrap().clone();
        let body = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(axum::body::to_bytes(response.into_body(), usize::MAX))
            .unwrap();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["code"], "request.internal_error");
        assert_eq!(problem["title"], "Internal error");
        assert_eq!(problem["type"], "/problems/request.internal_error");
        assert!(problem.get("detail").is_none());
        assert_eq!(
            report.code, "files.store_gone",
            "the log keeps the error's own code"
        );
    }

    #[test]
    fn err_08_an_internal_error_answers_the_fixed_body_and_reports_its_cause() {
        let response = Problem(Internal::new(Rejected)).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        let report = response.extensions().get::<ErrorReport>().unwrap();
        assert_eq!(report.code, "request.internal_error");
        assert_eq!(
            report.chain,
            "internal error: duplicate key value violates unique constraint"
        );
    }
}
