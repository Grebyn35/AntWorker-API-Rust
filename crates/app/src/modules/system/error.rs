//! What `system` reports (ERR-01): the failures of `seed`, which its subcommand writes, and which
//! no route answers, so they carry no code (ERR-02).

use axum::http::{Method, StatusCode};
use platform::errors::Internal;

/// A failure of `system`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SystemError {
    /// `ENVIRONMENT` isn't `development`, so `seed` refuses (SYSTEM-002).
    #[error(
        "seed runs only when ENVIRONMENT is development, since every account it registers has the \
         password it prints"
    )]
    NotDevelopment,
    /// An account that `seed` registers exists already, so the database holds its data
    /// (SYSTEM-004).
    #[error(
        "{0} is registered already, so this database holds the seed's data: it seeds a database \
         once, and again only after the database is recreated"
    )]
    Seeded(&'static str),
    /// The API refused one of the seed's requests.
    #[error("{method} {path} answered {status}: {body}")]
    Refused {
        /// The request's method.
        method: Method,
        /// The request's path.
        path: String,
        /// The answer's status.
        status: StatusCode,
        /// The answer's body.
        body: String,
    },
    /// Anything else, such as an answer that isn't the JSON it should be (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}
