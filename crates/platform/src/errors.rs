//! Errors shared by every module.

use std::{error::Error, fmt};

/// A failure in infrastructure, such as the database. It keeps its cause for the logs, and
/// nothing about the cause reaches a client (ERR-04, ERR-08).
pub struct Internal {
    cause: Box<dyn Error + Send + Sync>,
}

impl Internal {
    /// An internal error caused by `cause`.
    pub fn new(cause: impl Error + Send + Sync + 'static) -> Self {
        Self {
            cause: Box::new(cause),
        }
    }
}

impl From<sqlx::Error> for Internal {
    fn from(cause: sqlx::Error) -> Self {
        Self::new(cause)
    }
}

impl fmt::Display for Internal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("internal error")
    }
}

/// Shows the cause chain as `Display` text only. A cause's own `Debug` output can carry
/// personal data, such as the row a database constraint rejected (OBS-04).
impl fmt::Debug for Internal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Internal({})", chain(self.cause.as_ref()))
    }
}

impl Error for Internal {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// `error` and each of its causes, outermost first, joined with `: `.
pub fn chain(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("outer")]
    struct Outer(#[source] std::io::Error);

    #[test]
    fn err_10_a_chain_lists_every_cause_outermost_first() {
        let error = Outer(std::io::Error::other("inner"));
        assert_eq!(chain(&error), "outer: inner");
    }

    /// A cause whose `Debug` output holds personal data, as a database error's detail can.
    #[derive(thiserror::Error)]
    #[error("duplicate key value violates unique constraint")]
    struct Rejected;

    impl fmt::Debug for Rejected {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("Rejected { detail: \"Key (email)=(alice@example.com) already exists.\" }")
        }
    }

    #[test]
    fn obs_04_an_internal_error_never_prints_its_cause_through_debug() {
        let error = Internal::new(Rejected);
        let debug = format!("{error:?}");
        assert!(!debug.contains("alice@example.com"));
        assert_eq!(
            debug,
            "Internal(duplicate key value violates unique constraint)"
        );
    }
}
