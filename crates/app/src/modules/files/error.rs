//! What `files` reports (ERR-01).

use platform::errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError};

use crate::modules::audit::AuditError;

/// A failure of `files`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FilesError {
    /// The file ID names no file of the workspace (FILES-004).
    #[error("the file doesn't exist")]
    NotFound,
    /// `platform` refused the request, such as by authorization, its idempotency key or its
    /// input, or couldn't check it, or the update failed its precondition (PLATFORM-071).
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<RequestError> for FilesError {
    fn from(error: RequestError) -> Self {
        Self::Platform(error.into())
    }
}

impl From<AuditError> for FilesError {
    fn from(error: AuditError) -> Self {
        match error {
            AuditError::Internal(error) => Self::Internal(error),
        }
    }
}

impl ErrorCode for FilesError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "files.not_found",
            Self::Platform(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::NotFound => "File not found",
            Self::Platform(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::NotFound => Category::NotFound,
            Self::Platform(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::Platform(error) => error.detail(),
            _ => None,
        }
    }

    fn errors(&self) -> Option<Vec<FieldError>> {
        match self {
            Self::Platform(error) => error.errors(),
            _ => None,
        }
    }
}
