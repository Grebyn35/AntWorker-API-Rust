//! What `audit` reports (ERR-01).

use platform::errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError};

use crate::modules::access::AccessError;

/// A failure of `audit`'s routes. Recording an entry fails only internally, as `Internal`, which
/// fails the change it records too, since both share the unit of work (AUDIT-001).
#[derive(Debug, thiserror::Error)]
pub(crate) enum AuditError {
    /// `platform` refused the request, such as by authorization or by its query, or couldn't
    /// check it.
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// A query failed, or a stored role isn't one `access` knows (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<RequestError> for AuditError {
    fn from(error: RequestError) -> Self {
        Self::Platform(error.into())
    }
}

impl From<AccessError> for AuditError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Internal(error) => Self::Internal(error),
        }
    }
}

impl ErrorCode for AuditError {
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
            Self::Internal(error) => error.detail(),
        }
    }

    fn errors(&self) -> Option<Vec<FieldError>> {
        match self {
            Self::Platform(error) => error.errors(),
            Self::Internal(error) => error.errors(),
        }
    }
}
