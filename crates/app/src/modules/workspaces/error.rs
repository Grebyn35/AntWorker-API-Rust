//! What `workspaces` reports (ERR-01).

use platform::errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError};

use crate::modules::{access::AccessError, accounts::AccountsError, audit::AuditError};

/// A failure of `workspaces`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkspacesError {
    /// The caller tried to remove themselves (WORKSPACES-006).
    #[error("the caller can't remove themselves")]
    CannotRemoveSelf,
    /// The user isn't a member of the workspace (WORKSPACES-006).
    #[error("the user isn't a member of the workspace")]
    MemberNotFound,
    /// `platform` refused the request, such as by authorization, its idempotency key or its
    /// input, or couldn't check it.
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

impl ErrorCode for WorkspacesError {
    fn code(&self) -> &'static str {
        match self {
            Self::CannotRemoveSelf => "workspaces.cannot_remove_self",
            Self::MemberNotFound => "workspaces.member_not_found",
            Self::Platform(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::CannotRemoveSelf => "Can't remove yourself",
            Self::MemberNotFound => "Member not found",
            Self::Platform(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::CannotRemoveSelf => Category::BusinessRule,
            Self::MemberNotFound => Category::NotFound,
            Self::Platform(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::CannotRemoveSelf => {
                Some("Leave the workspace instead of removing yourself.".to_owned())
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
