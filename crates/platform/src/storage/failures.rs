//! What a call to the bucket's failure stands for: an object that isn't there, a bucket that
//! didn't answer, or a request it refused, each with a cause written here, never the library's
//! text, which names the request's URL, and with it the key (ERR-04).

use object_store::client::{HttpError, HttpErrorKind};

use super::{Call, StorageError};
use crate::errors::Internal;

/// The failure `error` stands for, with a cause written here, since the library's own text names
/// the request's URL, and with it the key (ERR-04): an object that isn't there; a bucket that
/// didn't answer, as the library's error of a connection, a request, a timeout or an interruption
/// says, or that answered a 5xx or a 429, which say it can't serve now; or a request it refused,
/// or an answer the client couldn't read.
pub(super) fn classify(error: &object_store::Error, call: Call) -> StorageError {
    let operation = call.operation();
    if let object_store::Error::NotFound { .. } = error {
        return StorageError::NotFound;
    }
    if let Some(kind) = transport(error) {
        let failure = format!("the request {operation} to the bucket failed: {kind:?}");
        return match kind {
            HttpErrorKind::Connect
            | HttpErrorKind::Request
            | HttpErrorKind::Timeout
            | HttpErrorKind::Interrupted => StorageError::Unavailable(Internal::new(failure)),
            _ => StorageError::Failed(Internal::new(failure)),
        };
    }
    if let Some(status) = status(error) {
        let failure = Internal::new(format!("the bucket answered {operation} with {status}"));
        return if status >= 500 || status == 429 {
            StorageError::Unavailable(failure)
        } else {
            StorageError::Failed(failure)
        };
    }
    let what = match error {
        object_store::Error::PermissionDenied { .. } => "refused the key pair's permission",
        object_store::Error::Unauthenticated { .. } => "refused the key pair",
        object_store::Error::Precondition { .. } => "refused a precondition",
        object_store::Error::AlreadyExists { .. } => "answered that the object exists",
        _ => "gave an answer the client couldn't read",
    };
    StorageError::Failed(Internal::new(format!("the bucket {what}, for {operation}")))
}

/// The kind of the connection's failure that `error` holds, if any.
fn transport(error: &object_store::Error) -> Option<HttpErrorKind> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = source {
        if let Some(failed) = error.downcast_ref::<HttpError>() {
            return Some(failed.kind());
        }
        source = error.source();
    }
    None
}

/// The status of the bucket's answer that `error` reports, if it reports one. The library keeps
/// the status in a type it doesn't export, so it is read from the error's text, `status code:`
/// and three digits, which the tests of FILES-012 pin.
fn status(error: &object_store::Error) -> Option<u16> {
    let text = error.to_string();
    let (_, after) = text.split_once("status code: ")?;
    after.get(..3)?.parse().ok()
}
