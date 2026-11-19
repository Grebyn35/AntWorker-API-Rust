//! What a call to the bucket's failure stands for: an object that isn't there, a bucket that
//! didn't answer, or a request it refused, each with a cause written here, never the HTTP
//! client's text, which names the request's URL, and with it the key and the signature (ERR-04).

use reqwest::StatusCode;

use super::{Call, StorageError};
use crate::errors::Internal;

/// What the bucket's `answer` to `call`, or the failure to get one, stands for: the answer, when
/// its status is a success; an object that isn't there, for a 404; a bucket that can't serve now,
/// for a 5xx or a 429; and a request that it refused, for any other status, a redirect's among
/// them, which the client doesn't follow.
pub(super) fn answered_with(
    answer: Result<reqwest::Response, reqwest::Error>,
    call: Call,
) -> Result<reqwest::Response, StorageError> {
    let answer = answer.map_err(|error| sent(&error, call))?;
    let status = answer.status();
    if status.is_success() {
        return Ok(answer);
    }
    if status == StatusCode::NOT_FOUND {
        return Err(StorageError::NotFound);
    }
    let failure = Internal::new(format!(
        "the bucket answered {} with {}",
        call.operation(),
        status.as_u16()
    ));
    Err(
        if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
            StorageError::Unavailable(failure)
        } else {
            StorageError::Failed(failure)
        },
    )
}

/// What `error`, the HTTP client's, of a request of `call` or of its answer's bytes, stands for:
/// a bucket that didn't answer, when the connection failed, the request couldn't be sent, or the
/// answer's bytes stopped; and a failed request otherwise. The cause is written here, from the
/// kind of the failure alone (ERR-04).
pub(super) fn sent(error: &reqwest::Error, call: Call) -> StorageError {
    let (kind, answering) = if error.is_timeout() {
        ("the connection timed out", false)
    } else if error.is_connect() {
        ("no connection opened", false)
    } else if error.is_body() || error.is_decode() {
        ("the bytes stopped coming", false)
    } else if error.is_request() {
        ("it couldn't be sent", false)
    } else {
        ("the client couldn't make it", true)
    };
    let failure = Internal::new(format!(
        "the request {} to the bucket failed: {kind}",
        call.operation()
    ));
    if answering {
        StorageError::Failed(failure)
    } else {
        StorageError::Unavailable(failure)
    }
}
