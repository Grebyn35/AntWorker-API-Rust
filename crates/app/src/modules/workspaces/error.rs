//! What `workspaces` reports (ERR-01).

use platform::errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError};

use crate::modules::{
    access::AccessError, accounts::AccountsError, audit::AuditError, auth::AuthError,
};
// part: files
use crate::modules::files::FilesError;
// end part: files

/// A failure of `workspaces`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkspacesError {
    // part: files
    /// The picture's body is empty, or no PNG, JPEG, GIF or WebP image (WORKSPACES-020).
    #[error("the picture isn't an image of a kind the API accepts")]
    PictureNotImage,
    /// The scanner found malware in the picture, or its contents pass what it scans whole
    /// (WORKSPACES-020).
    #[error("the scanner found malware in the picture")]
    PictureMalwareDetected,
    /// The scanner or the storage didn't answer (WORKSPACES-020). The documentation of a route
    /// lists it without its cause.
    #[error("the picture can't be stored now")]
    PictureUnavailable(#[source] Option<Internal>),
    // end part: files
    /// The caller tried to remove themselves (WORKSPACES-006).
    #[error("the caller can't remove themselves")]
    CannotRemoveSelf,
    /// The user isn't a member of the workspace (WORKSPACES-006, WORKSPACES-011, WORKSPACES-013,
    /// WORKSPACES-016).
    #[error("the user isn't a member of the workspace")]
    MemberNotFound,
    /// The member a transfer names isn't an admin (WORKSPACES-016).
    #[error("the member a transfer names isn't an admin")]
    TransferRequiresAdmin,
    /// `platform` refused the request, such as by authorization, its idempotency key or its
    /// input, or couldn't check it, or the update failed its precondition (PLATFORM-071).
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<RequestError> for WorkspacesError {
    fn from(error: RequestError) -> Self {
        Self::Platform(error.into())
    }
}

impl From<AccessError> for WorkspacesError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Internal(error) => Self::Internal(error),
        }
    }
}

impl From<AccountsError> for WorkspacesError {
    fn from(error: AccountsError) -> Self {
        match error {
            AccountsError::Internal(error) => Self::Internal(error),
            AccountsError::Platform(error) => Self::Platform(error),
            other => Self::Internal(Internal::new(other)),
        }
    }
}

impl From<AuthError> for WorkspacesError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Request(error) => Self::Platform(error.into()),
            AuthError::Internal(error) => Self::Internal(error),
            other => Self::Internal(Internal::new(other)),
        }
    }
}

impl From<AuditError> for WorkspacesError {
    fn from(error: AuditError) -> Self {
        match error {
            AuditError::Internal(error) => Self::Internal(error),
        }
    }
}

// part: files
impl From<FilesError> for WorkspacesError {
    /// A picture's upload refused as `files` refuses an upload, in this module's terms: what
    /// isn't a supported image is no image (WORKSPACES-020).
    fn from(error: FilesError) -> Self {
        match error {
            FilesError::Empty | FilesError::UnsupportedType => Self::PictureNotImage,
            FilesError::MalwareDetected => Self::PictureMalwareDetected,
            FilesError::ScannerUnavailable(cause) | FilesError::StorageUnavailable(cause) => {
                Self::PictureUnavailable(cause)
            }
            FilesError::Platform(error) => Self::Platform(error),
            FilesError::Internal(error) => Self::Internal(error),
            other => Self::Internal(Internal::new(other)),
        }
    }
}
// end part: files

impl ErrorCode for WorkspacesError {
    fn code(&self) -> &'static str {
        match self {
            // part: files
            Self::PictureNotImage => "workspaces.picture_not_image",
            Self::PictureMalwareDetected => "workspaces.picture_malware_detected",
            Self::PictureUnavailable(_) => "workspaces.picture_unavailable",
            // end part: files
            Self::CannotRemoveSelf => "workspaces.cannot_remove_self",
            Self::MemberNotFound => "workspaces.member_not_found",
            Self::TransferRequiresAdmin => "workspaces.transfer_requires_admin",
            Self::Platform(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            // part: files
            Self::PictureNotImage => "Picture not an image",
            Self::PictureMalwareDetected => "Malware detected",
            Self::PictureUnavailable(_) => "Picture unavailable",
            // end part: files
            Self::CannotRemoveSelf => "Can't remove yourself",
            Self::MemberNotFound => "Member not found",
            Self::TransferRequiresAdmin => "Only an admin can become the owner",
            Self::Platform(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            // part: files
            Self::PictureNotImage | Self::PictureMalwareDetected => Category::BusinessRule,
            Self::PictureUnavailable(_) => Category::Unavailable,
            // end part: files
            Self::CannotRemoveSelf | Self::TransferRequiresAdmin => Category::BusinessRule,
            Self::MemberNotFound => Category::NotFound,
            Self::Platform(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            // part: files
            Self::PictureNotImage => Some(
                "A workspace's picture is a PNG, JPEG, GIF or WebP image, of at most 1 MiB."
                    .to_owned(),
            ),
            Self::PictureMalwareDetected => {
                Some("The malware scanner refused the image, so it wasn't stored.".to_owned())
            }
            // end part: files
            Self::CannotRemoveSelf => {
                Some("Leave the workspace instead of removing yourself.".to_owned())
            }
            Self::TransferRequiresAdmin => {
                Some("A workspace goes to one of its admins: name a member who is one.".to_owned())
            }
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
