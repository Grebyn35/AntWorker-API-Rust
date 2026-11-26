//! What `access` reports (ERR-01).

use platform::errors::Internal;

/// A failure of `access`, which no route answers: each module that calls `access` takes it as its
/// own internal error.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AccessError {
    /// A query failed, or a stored role isn't one `access` knows (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}
