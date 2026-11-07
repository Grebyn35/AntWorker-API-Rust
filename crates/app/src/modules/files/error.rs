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
    /// The bucket didn't answer (FILES-012). The documentation of a route lists it without its
    /// cause.
    #[error("the storage is unavailable")]
    StorageUnavailable(#[source] Option<Internal>),
    /// The bucket is restoring from its copy, and doesn't hold the file's object yet
    /// (FILES-014).
    #[error("the file's bytes are being restored")]
    Restoring,
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
            Self::StorageUnavailable(_) => "files.storage_unavailable",
            Self::Restoring => "files.restoring",
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
            Self::StorageUnavailable(_) => "Storage unavailable",
            Self::Restoring => "File being restored",
            Self::Platform(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::NotFound => Category::NotFound,
            Self::InvalidName(_) | Self::Empty => Category::Validation,
            Self::UnsupportedType => Category::BusinessRule,
            Self::StorageUnavailable(_) | Self::Restoring => Category::Unavailable,
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
            Self::Restoring => Some(
                "The file's contents are coming back from a copy after a restore of the server."
                    .to_owned(),
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

    /// A minute while the bucket restores from its copy, which brings files back over minutes,
    /// and `platform`'s second otherwise (FILES-014).
    fn retry_after_seconds(&self) -> u32 {
        match self {
            Self::Restoring => 60,
            Self::Platform(error) => error.retry_after_seconds(),
            _ => 1,
        }
    }
}
