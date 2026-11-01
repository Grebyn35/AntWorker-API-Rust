//! What `mail` reports (ERR-01).

use platform::errors::{Category, ErrorCode, Internal};

/// A failure of `mail`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MailError {
    /// Enqueueing an email failed (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl ErrorCode for MailError {
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
