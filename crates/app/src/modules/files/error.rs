//! What `files` reports (ERR-01).

use platform::{
    errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError},
    types::Invalid,
};

use crate::modules::audit::AuditError;

/// A failure of `files`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FilesError {
    /// The file ID names no file of the workspace (FILES-008).
    #[error("the file doesn't exist")]
    NotFound,
    /// The upload names no file, or a name the rule refuses (FILES-002).
    #[error("the upload's name isn't one")]
    InvalidName(Option<Invalid>),
    /// The upload's body is empty (FILES-004).
    #[error("the upload is empty")]
    Empty,
    /// The upload's extension is no accepted kind's, or its bytes aren't of that kind
    /// (FILES-003).
    #[error("the upload isn't of a kind the API accepts")]
    UnsupportedType,
    /// The scanner found a signature in the upload, or its contents pass what it scans whole
    /// (FILES-005).
    #[error("the scanner found malware in the upload")]
    MalwareDetected,
    /// The scanner didn't answer (FILES-005). The documentation of a route lists it without its
    /// cause.
    #[error("the scanner is unavailable")]
    ScannerUnavailable(#[source] Option<Internal>),
    /// The bucket didn't answer (FILES-012). The documentation of a route lists it without its
    /// cause.
    #[error("the storage is unavailable")]
    StorageUnavailable(#[source] Option<Internal>),
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
            Self::InvalidName(_) => "files.invalid_name",
            Self::Empty => "files.empty",
            Self::UnsupportedType => "files.unsupported_type",
            Self::MalwareDetected => "files.malware_detected",
            Self::ScannerUnavailable(_) => "files.scanner_unavailable",
            Self::StorageUnavailable(_) => "files.storage_unavailable",
            Self::Platform(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::NotFound => "File not found",
            Self::InvalidName(_) => "Invalid file name",
            Self::Empty => "Empty file",
            Self::UnsupportedType => "Unsupported file type",
            Self::MalwareDetected => "Malware detected",
            Self::ScannerUnavailable(_) => "Scanner unavailable",
            Self::StorageUnavailable(_) => "Storage unavailable",
            Self::Platform(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::NotFound => Category::NotFound,
            Self::InvalidName(_) | Self::Empty => Category::Validation,
            Self::UnsupportedType | Self::MalwareDetected => Category::BusinessRule,
            Self::ScannerUnavailable(_) | Self::StorageUnavailable(_) => Category::Unavailable,
            Self::Platform(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::InvalidName(None) => Some(
                "Name the file in `Content-Disposition`, such as `attachment; filename*=UTF-8''report.pdf`."
                    .to_owned(),
            ),
            Self::InvalidName(Some(invalid)) => Some(invalid.message.clone().into_owned()),
            Self::UnsupportedType => Some(
                "The file's extension must be one the API accepts, and its contents of that type."
                    .to_owned(),
            ),
            Self::MalwareDetected => Some(
                "The malware scanner refused the file, so it wasn't stored.".to_owned(),
            ),
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
