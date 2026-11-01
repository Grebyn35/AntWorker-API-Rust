//! The errors `platform` answers itself, in the namespace `request` (ERR-02): those of the request
//! pipeline before any module runs, and those of the mechanisms a module calls, such as
//! authorization and idempotency keys.

use super::{Category, ErrorCode, FieldError, INTERNAL_CODE, INTERNAL_TITLE, Internal};

/// The errors `platform` answers itself, in the namespace `request` (ERR-02): before any module
/// runs, or in a mechanism a module calls, such as authorization and idempotency keys.
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    /// No route has the path.
    #[error("no route has the path")]
    NotFound,
    /// The path doesn't serve the method.
    #[error("the path doesn't serve the method")]
    MethodNotAllowed,
    /// The query holds parameters that the route's documentation doesn't list, or values the
    /// route refuses, each named by its JSON Pointer among the parameters (API-09, PLATFORM-035).
    #[error("the query holds parameters the route doesn't take")]
    InvalidQuery(Vec<FieldError>),
    /// The body isn't the JSON its route takes (PLATFORM-054).
    #[error("the body isn't the JSON the route takes")]
    InvalidBody(BodyFault),
    /// Fields of the body that their domain types refused (PLATFORM-056).
    #[error("fields of the request failed validation")]
    InvalidFields(Vec<FieldError>),
    /// The body is larger than the route's limit (API-08).
    #[error("the body is larger than the route's limit")]
    PayloadTooLarge,
    /// The request carries no access token, or one no session has (AUTH-009).
    #[error("the request isn't authenticated")]
    Unauthenticated,
    /// The request's access token has expired (AUTH-009).
    #[error("the access token has expired")]
    AccessTokenExpired,
    /// A request that changes something comes from an origin that isn't listed, or a cookie
    /// authenticates it and it names no origin (PLATFORM-053).
    #[error("the request's origin isn't allowed")]
    OriginNotAllowed,
    /// A member lacks the route's permission, or acts on a member of equal or higher rank
    /// (ACCESS-003, ACCESS-004).
    #[error("the caller's role doesn't allow the request")]
    PermissionDenied,
    /// The `Idempotency-Key` isn't a string of structured fields of 1 to 255 characters
    /// (PLATFORM-058).
    #[error("the idempotency key isn't valid")]
    InvalidIdempotencyKey,
    /// The key came with another request before (PLATFORM-018).
    #[error("the idempotency key was used for another request")]
    IdempotencyKeyReused,
    /// The key's first request hasn't committed or rolled back yet (PLATFORM-018).
    #[error("the idempotency key's first request is still running")]
    IdempotencyKeyInFlight,
    /// A rate limit's bucket is full (PLATFORM-048).
    #[error("a rate limit refused the request")]
    RateLimited,
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

/// Why a body isn't the JSON its route takes (PLATFORM-054).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyFault {
    /// The request's `Content-Type` isn't `application/json`.
    ContentType,
    /// The body isn't JSON.
    Syntax,
    /// The body is JSON, but not of the route's request type, such as with an unknown field. The
    /// field's JSON Pointer, when the parser names one.
    Shape(Option<String>),
}

impl ErrorCode for RequestError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "request.not_found",
            Self::MethodNotAllowed => "request.method_not_allowed",
            Self::InvalidQuery(_) => "request.invalid_query",
            Self::InvalidBody(_) => "request.invalid_body",
            Self::InvalidFields(_) => "request.invalid_fields",
            Self::PayloadTooLarge => "request.payload_too_large",
            Self::Unauthenticated => "request.unauthenticated",
            Self::AccessTokenExpired => "request.access_token_expired",
            Self::OriginNotAllowed => "request.origin_not_allowed",
            Self::PermissionDenied => "request.permission_denied",
            Self::InvalidIdempotencyKey => "request.invalid_idempotency_key",
            Self::IdempotencyKeyReused => "request.idempotency_key_reused",
            Self::IdempotencyKeyInFlight => "request.idempotency_key_in_flight",
            Self::RateLimited => "request.rate_limited",
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
            Self::InvalidBody(_) => "Invalid body",
            Self::InvalidFields(_) => "Invalid fields",
            Self::PayloadTooLarge => "Body too large",
            Self::Unauthenticated => "Not authenticated",
            Self::AccessTokenExpired => "Access token expired",
            Self::OriginNotAllowed => "Origin not allowed",
            Self::PermissionDenied => "Permission denied",
            Self::InvalidIdempotencyKey => "Invalid idempotency key",
            Self::IdempotencyKeyReused => "Idempotency key already used",
            Self::IdempotencyKeyInFlight => "A request is outstanding for this idempotency key",
            Self::RateLimited => "Too many requests",
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
            Self::InvalidQuery(_)
            | Self::InvalidBody(_)
            | Self::InvalidFields(_)
            | Self::InvalidIdempotencyKey => Category::Validation,
            Self::PayloadTooLarge => Category::PayloadTooLarge,
            Self::Unauthenticated | Self::AccessTokenExpired => Category::Unauthenticated,
            Self::OriginNotAllowed | Self::PermissionDenied => Category::Forbidden,
            Self::IdempotencyKeyReused => Category::BusinessRule,
            Self::IdempotencyKeyInFlight => Category::Conflict,
            Self::RateLimited => Category::RateLimited,
            Self::TimedOut | Self::Overloaded | Self::Unavailable => Category::Unavailable,
            Self::Panicked => Category::Internal,
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::InvalidQuery(_) => Some(
                "A query parameter isn't one the route takes, or has a value it refuses."
                    .to_owned(),
            ),
            Self::InvalidIdempotencyKey => {
                Some("Send Idempotency-Key as a quoted string of 1 to 255 characters.".to_owned())
            }
            Self::IdempotencyKeyReused => Some(
                "The key was sent before with another request: send a new key for this one."
                    .to_owned(),
            ),
            Self::IdempotencyKeyInFlight => {
                Some("The key's first request is still running: retry later.".to_owned())
            }
            Self::InvalidBody(BodyFault::ContentType) => {
                Some("The body must be sent as application/json.".to_owned())
            }
            Self::InvalidBody(BodyFault::Syntax) => Some("The body isn't valid JSON.".to_owned()),
            Self::InvalidBody(BodyFault::Shape(_)) => {
                Some("The body doesn't match what the route takes.".to_owned())
            }
            _ => None,
        }
    }

    fn errors(&self) -> Option<Vec<FieldError>> {
        match self {
            Self::InvalidBody(BodyFault::Shape(Some(field))) => Some(vec![FieldError::new(
                field.clone(),
                "invalid",
                "The field isn't one the route takes, or has the wrong type.",
            )]),
            Self::InvalidFields(fields) | Self::InvalidQuery(fields) => Some(fields.clone()),
            _ => None,
        }
    }
}

/// What `platform` reports to a module once its handler has begun: a request that it refused,
/// such as by authorization or an idempotency key, or a check that couldn't run.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// The request was refused.
    #[error(transparent)]
    Request(#[from] RequestError),
    /// The check failed (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl ErrorCode for PlatformError {
    fn code(&self) -> &'static str {
        match self {
            Self::Request(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Request(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::Request(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::Request(error) => error.detail(),
            Self::Internal(_) => None,
        }
    }

    fn errors(&self) -> Option<Vec<FieldError>> {
        match self {
            Self::Request(error) => error.errors(),
            Self::Internal(_) => None,
        }
    }
}

impl RequestError {
    /// One value of every variant, so the OpenAPI document can list every code `platform`
    /// answers (ERR-13). A new variant is added here too.
    pub fn every() -> [Self; 18] {
        [
            Self::NotFound,
            Self::MethodNotAllowed,
            Self::InvalidQuery(Vec::new()),
            Self::InvalidBody(BodyFault::Syntax),
            Self::InvalidFields(Vec::new()),
            Self::PayloadTooLarge,
            Self::Unauthenticated,
            Self::AccessTokenExpired,
            Self::OriginNotAllowed,
            Self::PermissionDenied,
            Self::InvalidIdempotencyKey,
            Self::IdempotencyKeyReused,
            Self::IdempotencyKeyInFlight,
            Self::RateLimited,
            Self::TimedOut,
            Self::Overloaded,
            Self::Unavailable,
            Self::Panicked,
        ]
    }
}
