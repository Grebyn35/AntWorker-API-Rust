//! What `accounts` reports (ERR-01).

use platform::errors::{Category, ErrorCode, Internal};

/// A failure of `accounts`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AccountsError {
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl ErrorCode for AccountsError {
    fn code(&self) -> &'static str {
        match self {
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::Internal(error) => error.category(),
        }
    }
}
