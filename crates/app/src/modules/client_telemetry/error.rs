//! What `client_telemetry` reports (ERR-01).

use platform::{
    errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError},
    rate_limit::LimitError,
};

/// A failure of `client_telemetry`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientTelemetryError {
    /// The caller doesn't consent to their clients' sending their telemetry, which a batch needs
    /// (CLIENT_TELEMETRY-005).
    #[error("the caller doesn't consent to their clients' telemetry")]
    ConsentRequired,
    /// The collector refused the batch, as it does a body that isn't the protocol's request
    /// (CLIENT_TELEMETRY-009).
    #[error("the collector refused the batch")]
    BatchRefused,
    /// The collector didn't take the batch (CLIENT_TELEMETRY-009). The documentation of a route
    /// lists it without its cause.
    #[error("the collector is unavailable")]
    CollectorUnavailable(#[source] Option<Internal>),
    /// A budget of bytes refused the batch, or couldn't count it (CLIENT_TELEMETRY-007,
    /// CLIENT_TELEMETRY-008).
    #[error(transparent)]
    Limit(#[from] LimitError),
    /// A refusal of the request pipeline's, such as a stale `ETag` (PLATFORM-071), or a body that
    /// isn't a batch (PLATFORM-090).
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// A query failed (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<RequestError> for ClientTelemetryError {
    fn from(error: RequestError) -> Self {
        Self::Platform(error.into())
    }
}

impl ErrorCode for ClientTelemetryError {
    fn code(&self) -> &'static str {
        match self {
            Self::ConsentRequired => "client_telemetry.consent_required",
            Self::BatchRefused => "client_telemetry.batch_refused",
            Self::CollectorUnavailable(_) => "client_telemetry.collector_unavailable",
            Self::Limit(error) => error.code(),
            Self::Platform(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::ConsentRequired => "Consent required",
            Self::BatchRefused => "Batch refused",
            Self::CollectorUnavailable(_) => "Collector unavailable",
            Self::Limit(error) => error.title(),
            Self::Platform(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::ConsentRequired => Category::Forbidden,
            Self::BatchRefused => Category::Validation,
            Self::CollectorUnavailable(_) => Category::Unavailable,
            Self::Limit(error) => error.category(),
            Self::Platform(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::ConsentRequired => Some(
                "The user hasn't consented to their clients' sending their telemetry.".to_owned(),
            ),
            Self::BatchRefused => {
                Some("The collector refused the batch: send the protocol's request.".to_owned())
            }
            Self::Platform(error) => error.detail(),
            Self::CollectorUnavailable(_) | Self::Limit(_) | Self::Internal(_) => None,
        }
    }

    fn errors(&self) -> Option<Vec<FieldError>> {
        match self {
            Self::Platform(error) => error.errors(),
            _ => None,
        }
    }
}
