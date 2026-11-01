//! What `invitations` reports (ERR-01).

use platform::errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError};

use crate::modules::{
    access::AccessError, accounts::AccountsError, audit::AuditError, mail::MailError,
    workspaces::WorkspacesError,
};

/// A failure of `invitations`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum InvitationsError {
    /// The invitee is a member of the workspace already (INVITATIONS-002, INVITATIONS-006).
    #[error("the invitee is a member of the workspace already")]
    AlreadyMember,
    /// The invitation expired (INVITATIONS-006).
    #[error("the invitation expired")]
    Expired,
    /// No invitation with the ID is addressed to the caller (INVITATIONS-006).
    #[error("no invitation with the ID is addressed to the caller")]
    NotFound,
    /// `platform` refused the request, such as by authorization, its idempotency key or its
    /// input, or couldn't check it.
    #[error(transparent)]
    Platform(#[from] PlatformError),
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<RequestError> for InvitationsError {
    fn from(error: RequestError) -> Self {
        Self::Platform(error.into())
    }
}

impl From<AccessError> for InvitationsError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Internal(error) => Self::Internal(error),
        }
    }
}

impl From<AccountsError> for InvitationsError {
    fn from(error: AccountsError) -> Self {
        match error {
            AccountsError::Internal(error) => Self::Internal(error),
        }
    }
}

impl From<AuditError> for InvitationsError {
    fn from(error: AuditError) -> Self {
        match error {
            AuditError::Internal(error) => Self::Internal(error),
        }
    }
}

impl From<MailError> for InvitationsError {
    fn from(error: MailError) -> Self {
        match error {
            MailError::Internal(error) => Self::Internal(error),
        }
    }
}

impl From<WorkspacesError> for InvitationsError {
    /// The functions of `workspaces` that this module calls fail only internally.
    fn from(error: WorkspacesError) -> Self {
        match error {
            WorkspacesError::Internal(error) => Self::Internal(error),
            WorkspacesError::Platform(error) => Self::Platform(error),
            other => Self::Internal(Internal::new(other)),
        }
    }
}

impl ErrorCode for InvitationsError {
    fn code(&self) -> &'static str {
        match self {
            Self::AlreadyMember => "invitations.already_member",
            Self::Expired => "invitations.expired",
            Self::NotFound => "invitations.not_found",
            Self::Platform(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::AlreadyMember => "Already a member",
            Self::Expired => "Invitation expired",
            Self::NotFound => "Invitation not found",
            Self::Platform(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::AlreadyMember => Category::Conflict,
            Self::Expired => Category::BusinessRule,
            Self::NotFound => Category::NotFound,
            Self::Platform(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::Expired => Some("Ask a member of the workspace to invite you again.".to_owned()),
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
