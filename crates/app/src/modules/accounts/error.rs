//! What `accounts` reports (ERR-01).

use platform::{
    errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError},
    rate_limit::LimitError,
};

use crate::modules::{access::AccessError, audit::AuditError};
// part: files
use crate::modules::files::FilesError;
// end part: files

/// A failure of `accounts`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AccountsError {
    // part: files
    /// The avatar's body is empty, or no PNG, JPEG, GIF or WebP image (ACCOUNTS-009).
    #[error("the avatar isn't an image of a kind the API accepts")]
    AvatarNotImage,
    /// The scanner found malware in the avatar, or its contents pass what it scans whole
    /// (ACCOUNTS-009).
    #[error("the scanner found malware in the avatar")]
    AvatarMalwareDetected,
    /// The scanner or the storage didn't answer (ACCOUNTS-009). The documentation of a route
    /// lists it without its cause.
    #[error("the avatar can't be stored now")]
    AvatarUnavailable(#[source] Option<Internal>),
    // end part: files
    /// The export names no export of the caller's, an expired one among them (ACCOUNTS-022).
    #[error("the export doesn't exist")]
    ExportNotFound,
    /// The export's archive isn't built, or never will be (ACCOUNTS-026).
    #[error("the export isn't ready")]
    ExportNotReady,
    /// The caller's bucket of exports is full (ACCOUNTS-021), or couldn't count the request.
    #[error(transparent)]
    Limit(#[from] LimitError),
    /// `platform` refused the request, such as by its input, or couldn't check it, or the update
    /// failed its precondition (PLATFORM-071).
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<RequestError> for AccountsError {
    fn from(error: RequestError) -> Self {
        Self::Platform(error.into())
    }
}

impl From<AccessError> for AccountsError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Internal(error) => Self::Internal(error),
        }
    }
}

impl From<AuditError> for AccountsError {
    fn from(error: AuditError) -> Self {
        match error {
            AuditError::Internal(error) => Self::Internal(error),
        }
    }
}

// part: files
impl From<FilesError> for AccountsError {
    /// An avatar's upload refused as `files` refuses an upload, in this module's terms: what
    /// isn't a supported image is no image (ACCOUNTS-009).
    fn from(error: FilesError) -> Self {
        match error {
            FilesError::Empty | FilesError::UnsupportedType => Self::AvatarNotImage,
            FilesError::MalwareDetected => Self::AvatarMalwareDetected,
            FilesError::ScannerUnavailable(cause) | FilesError::StorageUnavailable(cause) => {
                Self::AvatarUnavailable(cause)
            }
            FilesError::Platform(error) => Self::Platform(error),
            FilesError::Internal(error) => Self::Internal(error),
            other => Self::Internal(Internal::new(other)),
        }
    }
}
// end part: files

impl ErrorCode for AccountsError {
    fn code(&self) -> &'static str {
        match self {
            // part: files
            Self::AvatarNotImage => "accounts.avatar_not_image",
            Self::AvatarMalwareDetected => "accounts.avatar_malware_detected",
            Self::AvatarUnavailable(_) => "accounts.avatar_unavailable",
            // end part: files
            Self::ExportNotFound => "accounts.export_not_found",
            Self::ExportNotReady => "accounts.export_not_ready",
            Self::Limit(error) => error.code(),
            Self::Platform(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            // part: files
            Self::AvatarNotImage => "Avatar not an image",
            Self::AvatarMalwareDetected => "Malware detected",
            Self::AvatarUnavailable(_) => "Avatar unavailable",
            // end part: files
            Self::ExportNotFound => "Export not found",
            Self::ExportNotReady => "Export not ready",
            Self::Limit(error) => error.title(),
            Self::Platform(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            // part: files
            Self::AvatarNotImage | Self::AvatarMalwareDetected => Category::BusinessRule,
            Self::AvatarUnavailable(_) => Category::Unavailable,
            // end part: files
            Self::ExportNotFound => Category::NotFound,
            Self::ExportNotReady => Category::Conflict,
            Self::Limit(error) => error.category(),
            Self::Platform(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            // part: files
            Self::AvatarNotImage => {
                Some("An avatar is a PNG, JPEG, GIF or WebP image, of at most 1 MiB.".to_owned())
            }
            Self::AvatarMalwareDetected => {
                Some("The malware scanner refused the image, so it wasn't stored.".to_owned())
            }
            // end part: files
            Self::ExportNotReady => Some(
                "The export's archive isn't built yet, or its job failed and it never will be."
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
}
