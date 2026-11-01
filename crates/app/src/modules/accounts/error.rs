//! What `accounts` reports (ERR-01).

use platform::errors::{Category, ErrorCode, Internal, RequestError};

/// A failure of `accounts`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AccountsError {
    /// Another user has the email (ACCOUNTS-004).
    #[error("another user has the email")]
    EmailInUse,
    /// The request failed before `accounts` ran, such as its authentication.
    #[error(transparent)]
    Request(#[from] RequestError),
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl ErrorCode for AccountsError {
    fn code(&self) -> &'static str {
        match self {
            Self::EmailInUse => "accounts.email_in_use",
            Self::Request(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::EmailInUse => "Email in use",
            Self::Request(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::EmailInUse => Category::Conflict,
            Self::Request(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }
}
