//! What `superadmins` reports (ERR-01).

use platform::errors::{Category, ErrorCode, Internal};

/// A failure of `superadmins`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SuperadminsError {
    /// A query failed (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl ErrorCode for SuperadminsError {
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
