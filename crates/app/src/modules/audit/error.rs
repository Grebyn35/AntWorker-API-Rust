//! What `audit` reports (ERR-01).

use platform::errors::{Category, ErrorCode, Internal};

/// A failure of `audit`: an entry that couldn't be recorded, which fails the change it records
/// too, since both share the unit of work (AUDIT-001).
#[derive(Debug, thiserror::Error)]
pub(crate) enum AuditError {
    /// The entry's insert failed (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl ErrorCode for AuditError {
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
