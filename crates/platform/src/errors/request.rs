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
    /// route refuses, each named by its JSON Pointer among the parameters (API-09, PLATFORM-045).
    #[error("the query holds parameters the route doesn't take")]
    InvalidQuery(Vec<FieldError>),
    /// The body isn't the JSON its route takes (PLATFORM-049).
    #[error("the body isn't the JSON the route takes")]
    InvalidBody(BodyFault),
    /// Fields of the body that their domain types refused (PLATFORM-051).
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
    // part: files
    /// The request's signed URL is past its time (PLATFORM-080).
    #[error("the signed URL has expired")]
    SignedUrlExpired,
    // end part: files
    /// The caller's account hasn't proved its email address, which the route needs, while the
    /// deployment sends email (AUTH-025).
    #[error("the caller's email address isn't verified")]
    EmailUnverified,
    /// The caller's account has no second factor, which the deployment requires of every account
    /// and the route needs (AUTH-070).
    #[error("the caller's account has no second factor")]
    SecondFactorSetupRequired,
    /// A request that changes something comes from an origin that isn't listed, or a cookie
    /// authenticates it and it names no origin (PLATFORM-059).
    #[error("the request's origin isn't allowed")]
    OriginNotAllowed,
    /// A member lacks the route's permission, or acts on a member of equal or higher rank
    /// (ACCESS-003, ACCESS-004).
    #[error("the caller's role doesn't allow the request")]
    PermissionDenied,
    /// The caller's session hasn't proved who its user is within the last 5 minutes, which the
    /// route's action needs (AUTH-023).
    #[error("the caller's session hasn't just proved who its user is")]
    ReauthenticationRequired,
    /// The `Idempotency-Key` isn't a string of structured fields of 1 to 255 characters
    /// (PLATFORM-037).
    #[error("the idempotency key isn't valid")]
    InvalidIdempotencyKey,
    /// The key came with another request before (PLATFORM-036).
    #[error("the idempotency key was used for another request")]
    IdempotencyKeyReused,
    /// The key's first request hasn't committed or rolled back yet (PLATFORM-036).
    #[error("the idempotency key's first request is still running")]
    IdempotencyKeyInFlight,
    /// An update's `If-Match` names no current version of the resource (PLATFORM-071).
    #[error("the resource's version isn't one the request's If-Match names")]
    PreconditionFailed,
    /// A rate limit's bucket is full (PLATFORM-054).
    #[error("a rate limit refused the request")]
    RateLimited,
    /// The request ran past its timeout (OPS-07).
    #[error("the request ran past its timeout")]
    TimedOut,
    /// The route's feature is turned off, such as anything that needs email while the deployment
    /// sends none (OPS-04).
    #[error("the route's feature is turned off")]
    FeatureDisabled,
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

/// Why a body isn't the JSON its route takes (PLATFORM-049).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BodyFault {
    /// The request's `Content-Type` isn't the media type its route takes, which this names.
    ContentType(&'static str),
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
            // part: files
            Self::SignedUrlExpired => "request.signed_url_expired",
            // end part: files
            Self::EmailUnverified => "request.email_unverified",
            Self::SecondFactorSetupRequired => "request.second_factor_setup_required",
            Self::OriginNotAllowed => "request.origin_not_allowed",
            Self::PermissionDenied => "request.permission_denied",
            Self::ReauthenticationRequired => "request.reauthentication_required",
            Self::InvalidIdempotencyKey => "request.invalid_idempotency_key",
            Self::IdempotencyKeyReused => "request.idempotency_key_reused",
            Self::IdempotencyKeyInFlight => "request.idempotency_key_in_flight",
            Self::PreconditionFailed => "request.precondition_failed",
            Self::RateLimited => "request.rate_limited",
            Self::TimedOut => "request.timed_out",
            Self::FeatureDisabled => "request.feature_disabled",
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
            // part: files
            Self::SignedUrlExpired => "Signed URL expired",
            // end part: files
            Self::EmailUnverified => "Email address not verified",
            Self::SecondFactorSetupRequired => "Second factor required",
            Self::OriginNotAllowed => "Origin not allowed",
            Self::PermissionDenied => "Permission denied",
            Self::ReauthenticationRequired => "Re-authentication required",
            Self::InvalidIdempotencyKey => "Invalid idempotency key",
            Self::IdempotencyKeyReused => "Idempotency key already used",
            Self::IdempotencyKeyInFlight => "A request is outstanding for this idempotency key",
            Self::PreconditionFailed => "Precondition failed",
            Self::RateLimited => "Too many requests",
            Self::TimedOut => "Request timed out",
            Self::FeatureDisabled => "Feature turned off",
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
            // part: files
            Self::SignedUrlExpired => Category::Unauthenticated,
            // end part: files
            Self::EmailUnverified
            | Self::SecondFactorSetupRequired
            | Self::OriginNotAllowed
            | Self::PermissionDenied
            | Self::ReauthenticationRequired => Category::Forbidden,
            Self::IdempotencyKeyReused => Category::BusinessRule,
            Self::IdempotencyKeyInFlight => Category::Conflict,
            Self::PreconditionFailed => Category::PreconditionFailed,
            Self::RateLimited => Category::RateLimited,
            Self::TimedOut | Self::FeatureDisabled | Self::Overloaded | Self::Unavailable => {
                Category::Unavailable
            }
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
            Self::EmailUnverified => Some(
                "Verify the account's email address with the code sent to it first.".to_owned(),
            ),
            Self::SecondFactorSetupRequired => Some(
                "The deployment requires a second factor: set up an authenticator app first."
                    .to_owned(),
            ),
            Self::ReauthenticationRequired => {
                Some("Prove who you are again, then send the request again.".to_owned())
            }
            Self::FeatureDisabled => {
                Some("The deployment has turned this feature off.".to_owned())
            }
            // part: files
            Self::SignedUrlExpired => Some(
                "The URL's time has passed: read the file again for a fresh one.".to_owned(),
            ),
            // end part: files
            Self::PreconditionFailed => Some(
                "If-Match names no current version of the resource: read it again, and send its ETag."
                    .to_owned(),
            ),
            Self::InvalidBody(BodyFault::ContentType(media_type)) => {
                Some(format!("The body must be sent as {media_type}."))
            }
            Self::InvalidBody(BodyFault::Syntax) => Some("The body isn't valid JSON.".to_owned()),
            Self::InvalidBody(BodyFault::Shape(_)) => {
                Some("The body doesn't match what the route takes.".to_owned())
            }
            _ => None,
        }
    }

    /// An hour for a feature the deployment turned off, which only a deploy turns on again
    /// (OPS-04), and a second for the rest.
    fn retry_after_seconds(&self) -> u32 {
        match self {
            Self::FeatureDisabled => 3_600,
            _ => 1,
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

    fn retry_after_seconds(&self) -> u32 {
        match self {
            Self::Request(error) => error.retry_after_seconds(),
            Self::Internal(error) => error.retry_after_seconds(),
        }
    }
}

impl RequestError {
    /// One value of every variant, so the OpenAPI document can list every code `platform`
    /// answers (ERR-13). A new variant is added here too.
    pub fn every() -> Vec<Self> {
        let mut every = vec![
            Self::NotFound,
            Self::MethodNotAllowed,
            Self::InvalidQuery(Vec::new()),
            Self::InvalidBody(BodyFault::Syntax),
            Self::InvalidFields(Vec::new()),
            Self::PayloadTooLarge,
            Self::Unauthenticated,
            Self::AccessTokenExpired,
        ];
        // part: files
        every.push(Self::SignedUrlExpired);
        // end part: files
        every.extend([
            Self::EmailUnverified,
            Self::SecondFactorSetupRequired,
            Self::OriginNotAllowed,
            Self::PermissionDenied,
            Self::ReauthenticationRequired,
            Self::InvalidIdempotencyKey,
            Self::IdempotencyKeyReused,
            Self::IdempotencyKeyInFlight,
            Self::PreconditionFailed,
            Self::RateLimited,
            Self::TimedOut,
            Self::FeatureDisabled,
            Self::Overloaded,
            Self::Unavailable,
            Self::Panicked,
        ]);
        every
    }
}
