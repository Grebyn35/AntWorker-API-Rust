//! What `access` reports (ERR-01).

use platform::errors::{Category, ErrorCode, Internal};

/// A failure of `access`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AccessError {
    /// A query failed, or a stored role isn't one `access` knows (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl ErrorCode for AccessError {
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
