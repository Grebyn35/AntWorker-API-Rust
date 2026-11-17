//! What `client_telemetry` reports (ERR-01).

use platform::errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError};

/// A failure of `client_telemetry`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClientTelemetryError {
    /// A refusal of the request pipeline's, such as a stale `ETag` (PLATFORM-071).
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
            Self::Platform(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Platform(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::Platform(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::Platform(error) => error.detail(),
            Self::Internal(_) => None,
        }
    }

    fn errors(&self) -> Option<Vec<FieldError>> {
        match self {
            Self::Platform(error) => error.errors(),
            Self::Internal(_) => None,
        }
    }
}
