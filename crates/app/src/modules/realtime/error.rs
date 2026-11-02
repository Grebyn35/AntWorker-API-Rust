//! What `realtime` reports (ERR-01).

use platform::{
    errors::{Category, ErrorCode, Internal},
    events::SubscribeError,
};

/// A failure of `realtime`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RealtimeError {
    /// The caller holds the most open streams a user may on the replica (REALTIME-006).
    #[error("the caller holds the most open streams a user may")]
    TooManyStreams,
    /// The replica is shutting down, so its hub opens no stream.
    #[error("the replica's hub can't deliver events")]
    Unavailable,
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<SubscribeError> for RealtimeError {
    fn from(error: SubscribeError) -> Self {
        match error {
            SubscribeError::TooManyStreams => Self::TooManyStreams,
            SubscribeError::Unavailable => Self::Unavailable,
            SubscribeError::Internal(error) => Self::Internal(error),
        }
    }
}

impl ErrorCode for RealtimeError {
    fn code(&self) -> &'static str {
        match self {
            Self::TooManyStreams => "realtime.too_many_streams",
            Self::Unavailable => "realtime.unavailable",
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::TooManyStreams => "Too many open streams",
            Self::Unavailable => "Events unavailable",
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::TooManyStreams => Category::Conflict,
            Self::Unavailable => Category::Unavailable,
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::TooManyStreams => {
                Some("Close a stream this account holds open before opening another.".to_owned())
            }
            _ => None,
        }
    }
}
