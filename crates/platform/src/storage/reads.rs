//! The reads of an object: its bytes, whole or a range of them, as the bucket streams them, and
//! its size alone (PLATFORM-094).

use std::ops::Range;

use futures_util::{StreamExt, stream};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use rusty_s3::S3Action;

use super::{
    Call, Object, ObjectKey, Storage, StorageError, URL_LIFETIME, answered, count, failures::sent,
};
use crate::errors::Internal;

impl Storage {
    /// The object `key`'s bytes, or those of `range` alone, offsets of its bytes that it holds,
    /// as the bucket streams them. The bucket has as long for each next chunk as for its first
    /// answer, counted from when the chunk is asked for, so a client that reads slowly never cuts
    /// its download short.
    pub async fn get(
        &self,
        key: &ObjectKey,
        range: Option<Range<u64>>,
    ) -> Result<Object, StorageError> {
        let request = key.addressed().and_then(|object| {
            let action = self.bucket.get_object(Some(&self.credentials), object);
            let request = self.client.get(action.sign(URL_LIFETIME));
            match &range {
                None => Ok(request),
                Some(range) if range.start < range.end => {
                    // S3's `Range` names its last byte, where the range names the one after it.
                    let bytes = format!("bytes={}-{}", range.start, range.end - 1);
                    Ok(request.header(RANGE, bytes))
                }
                Some(_) => Err(StorageError::Failed(Internal::new(
                    "a read asked the bucket for a range of no bytes",
                ))),
            }
        });
        let ranged = range.is_some();
        let (found, size) = self
            .answer_reading(Call::Get, request, |found| size(found, ranged))
            .await?;
        Ok(self.streamed(found, size))
    }

    /// The object that the bucket's answer `found` to a read holds, whose whole holds `size`
    /// bytes: its bytes, as the bucket streams them.
    pub(super) fn streamed(&self, found: reqwest::Response, size: u64) -> Object {
        let chunks = stream::unfold(found, |mut found| async move {
            match found.chunk().await {
                Ok(Some(chunk)) => Some((Ok(chunk), found)),
                Ok(None) => None,
                Err(error) => Some((Err(sent(&error, Call::Get)), found)),
            }
        });
        // A stream that the bucket stops sending, or that fails, counts again, by its failure,
        // so a bucket that answers and then stalls still shows (OBS-14).
        let bytes = answered(chunks, self.clock.clone(), Call::Get)
            .map(|chunk| {
                chunk.map_err(|failure| {
                    count(Some(&failure));
                    failure.into_cause()
                })
            })
            .boxed();
        Object { size, bytes }
    }

    /// How many bytes the object `key` holds, as the bucket's answer to S3's `HeadObject` names
    /// them, which sends none of them.
    pub async fn head(&self, key: &ObjectKey) -> Result<u64, StorageError> {
        let request = key.addressed().map(|object| {
            let action = self.bucket.head_object(Some(&self.credentials), object);
            self.client.head(action.sign(URL_LIFETIME))
        });
        self.answer_reading(Call::Head, request, named_length)
            .await
            .map(|(_, length)| length)
    }
}

/// How many bytes the whole object of the answer `found` holds: what follows the slash of its
/// `Content-Range`, when the read asked for a range, which the bucket answers with a 206, and
/// its `Content-Length` otherwise.
pub(super) fn size(found: &reqwest::Response, ranged: bool) -> Result<u64, StorageError> {
    if !ranged {
        return found
            .content_length()
            .ok_or_else(|| unread(Call::Get, "no length of its bytes"));
    }
    if found.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err(unread(
            Call::Get,
            "the whole object, where a range was asked for",
        ));
    }
    found
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|range| range.to_str().ok())
        .and_then(whole)
        .ok_or_else(|| unread(Call::Get, "a range that names no size of the whole"))
}

/// How many bytes the object of `found`, the bucket's answer to a `HeadObject`, holds: the
/// number that its `Content-Length` header names, since the answer carries no body whose length
/// the HTTP client could give.
fn named_length(found: &reqwest::Response) -> Result<u64, StorageError> {
    found
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok())
        .and_then(number)
        .ok_or_else(|| unread(Call::Head, "no length of the object's bytes"))
}

/// The failure of an answer to `call` that names no size the client can read: `what` says what
/// it held instead.
fn unread(call: Call, what: &str) -> StorageError {
    StorageError::Failed(Internal::new(format!(
        "the bucket answered {} with {what}",
        call.operation()
    )))
}

/// The number that `text` writes in decimal digits alone, if it fits.
fn number(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// The size of the whole that a `Content-Range` names, such as the 1000 of `bytes 10-19/1000`,
/// when its first byte and its last are in that order and within the whole.
fn whole(content_range: &str) -> Option<u64> {
    let (range, whole) = content_range.strip_prefix("bytes ")?.split_once('/')?;
    let (first, last) = range.split_once('-')?;
    let (first, last, whole) = (number(first)?, number(last)?, number(whole)?);
    (first <= last && last < whole).then_some(whole)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_094_an_object_s_size_is_the_length_that_a_head_s_answer_names_in_digits() {
        let answer = |length: Option<&str>| {
            let mut answer = axum::http::Response::builder().status(200);
            if let Some(length) = length {
                answer = answer.header(CONTENT_LENGTH, length);
            }
            reqwest::Response::from(answer.body("").unwrap())
        };

        assert_eq!(named_length(&answer(Some("11"))).ok(), Some(11));
        assert_eq!(named_length(&answer(Some("0"))).ok(), Some(0));
        for unread in [
            None,
            Some(""),
            Some("-1"),
            Some("+1"),
            Some("1 1"),
            Some("11, 11"),
        ] {
            let failure = named_length(&answer(unread)).unwrap_err();
            assert!(matches!(failure, StorageError::Failed(_)), "{unread:?}");
        }
    }

    #[test]
    fn platform_094_a_ranged_answer_names_the_size_of_the_whole_after_its_slash() {
        assert_eq!(whole("bytes 10-19/1000"), Some(1000));
        assert_eq!(whole("bytes 0-0/1"), Some(1));
        for unread in [
            "bytes 10-19/*",
            "bytes */1000",
            "bytes 10-19",
            "bytes 19-10/1000",
            "bytes 10-1000/1000",
            "bytes -19/1000",
            "bytes 10-/1000",
            "bytes +1-+2/1000",
            "items 10-19/1000",
            "10-19/1000",
            "",
        ] {
            assert_eq!(whole(unread), None, "{unread}");
        }
    }
}
