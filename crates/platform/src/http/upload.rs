//! The body of an upload route: a file's bytes, read in one of the replica's turns, with the
//! request's idempotency key (PLATFORM-074).

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{FromRequest, Request},
    http::{HeaderValue, header},
};
use http_body_util::{BodyExt, Limited};
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{
    body::{is_media_type, is_too_large},
    docs::IDEMPOTENCY_KEY,
};
use crate::{
    blocking::Workers,
    errors::{BodyFault, Internal, PlatformError, Problem, RequestError},
    idempotency::IdempotencyKey,
};

/// The media type of an upload's bytes, whose kind its route finds from the bytes themselves.
const OCTET_STREAM: &str = "application/octet-stream";

/// How many uploads' bodies a replica holds in memory at once (PLATFORM-074).
const UPLOADS_AT_ONCE: usize = 4;

/// The turns of the uploads a replica reads at once, which the stack shares with every request
/// (PLATFORM-074).
#[derive(Clone, Debug)]
pub(super) struct UploadTurns(Arc<Semaphore>);

impl UploadTurns {
    /// Every turn free.
    pub(super) fn new() -> Self {
        Self(Arc::new(Semaphore::new(UPLOADS_AT_ONCE)))
    }
}

/// The body of an upload route: a file's bytes, sent as `application/octet-stream`, the one kind
/// of body that isn't JSON (API-08), which the handler reads once it has refused what it refuses without them, such as a caller
/// who may not upload (PLATFORM-073), up to the limit its module sets, in one of the replica's
/// turns, with the request's `Idempotency-Key`, if it sent one (PLATFORM-074). An upload route's
/// group sets no body limit of its own, so nothing reads the body before the handler does.
#[derive(Debug)]
pub struct Upload {
    body: Body,
    /// The body's declared length: `None` without `Content-Length`, and `Some(None)` for one that
    /// isn't a number Rust can hold.
    declared: Option<Option<usize>>,
    method: String,
    path: String,
    query: String,
    key: Option<HeaderValue>,
    /// The request's `Content-Disposition`, which names the file, and so is part of a keyed
    /// upload's fingerprint (API-12).
    disposition: Option<HeaderValue>,
    turns: UploadTurns,
}

impl<S: Send + Sync> FromRequest<S> for Upload {
    type Rejection = Problem<PlatformError>;

    async fn from_request(request: Request, _: &S) -> Result<Self, Self::Rejection> {
        let Some(turns) = request.extensions().get::<UploadTurns>().cloned() else {
            return Err(Problem(
                Internal::new("the request carries no upload turns").into(),
            ));
        };
        if !is_media_type(request.headers().get(header::CONTENT_TYPE), OCTET_STREAM) {
            return Err(Problem(
                RequestError::InvalidBody(BodyFault::ContentType(OCTET_STREAM)).into(),
            ));
        }
        let key = request.headers().get(IDEMPOTENCY_KEY).cloned();
        if key
            .as_ref()
            .is_some_and(|value| !IdempotencyKey::is_key(value.as_bytes()))
        {
            return Err(Problem(RequestError::InvalidIdempotencyKey.into()));
        }
        let declared = request.headers().get(header::CONTENT_LENGTH).map(|length| {
            length
                .to_str()
                .ok()
                .and_then(|length| length.parse::<usize>().ok())
        });
        Ok(Self {
            declared,
            disposition: request.headers().get(header::CONTENT_DISPOSITION).cloned(),
            method: request.method().as_str().to_owned(),
            path: request.uri().path().to_owned(),
            query: request.uri().query().unwrap_or_default().to_owned(),
            key,
            turns,
            body: request.into_body(),
        })
    }
}

/// An upload's bytes, which [`Upload::read`] read, and its key, with the fingerprint of its
/// method, path, query, `Content-Disposition` and bytes (PLATFORM-036, API-12). It holds the replica's turn until it is dropped,
/// so the handler keeps it until it has done with the bytes (PLATFORM-074).
#[derive(Debug)]
pub struct Uploaded {
    /// The bytes.
    pub bytes: Bytes,
    /// The key, which the operation's unit of work claims.
    pub key: Option<IdempotencyKey>,
    _turn: OwnedSemaphorePermit,
}

impl Upload {
    /// 413 `request.payload_too_large` when the body's `Content-Length` declares more than
    /// `limit` bytes, before anything reads it (PLATFORM-048).
    pub fn refuse_declared(&self, limit: usize) -> Result<(), RequestError> {
        match self.declared {
            Some(Some(length)) if length <= limit => Ok(()),
            Some(_) => Err(RequestError::PayloadTooLarge),
            None => Ok(()),
        }
    }

    /// Waits for one of the replica's turns, then reads the body, up to `limit` bytes: 413
    /// `request.payload_too_large` for a body that declares more or grows past it (PLATFORM-048,
    /// PLATFORM-050), and 400 `request.invalid_body` for one that stops before its end. A keyed
    /// upload's digest of the bytes, for its key's fingerprint, is CPU-heavy work, which runs
    /// through `workers` (CODE-05).
    pub async fn read(self, limit: usize, workers: &Workers) -> Result<Uploaded, PlatformError> {
        self.refuse_declared(limit)?;
        // The semaphore is never closed, so waiting for a turn ends only with one.
        let turn = self.turns.0.acquire_owned().await.map_err(Internal::new)?;
        let bytes = match Limited::new(self.body, limit).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(error) if is_too_large(&*error) => return Err(RequestError::PayloadTooLarge.into()),
            Err(_) => return Err(RequestError::InvalidBody(BodyFault::Syntax).into()),
        };
        let key = match self.key {
            None => None,
            Some(value) => {
                let digested = bytes.clone();
                let digest: [u8; 32] = workers
                    .run("upload digest", move || Sha256::digest(&digested).into())
                    .await
                    .map_err(Internal::new)?;
                let named = self
                    .disposition
                    .as_ref()
                    .map_or(&b""[..], HeaderValue::as_bytes);
                let key = IdempotencyKey::parse(
                    value.as_bytes(),
                    &self.method,
                    &self.path,
                    &self.query,
                    named,
                    &digest,
                )
                .ok_or(RequestError::InvalidIdempotencyKey)?;
                Some(key)
            }
        };
        Ok(Uploaded {
            bytes,
            key,
            _turn: turn,
        })
    }
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;

    use super::*;

    /// An upload of `body`, sharing `turns`, as the extractor makes it.
    fn upload(body: &'static str, turns: &UploadTurns) -> Upload {
        Upload {
            body: Body::from(body),
            declared: None,
            method: "POST".to_owned(),
            path: "/v1/uploads".to_owned(),
            query: String::new(),
            key: None,
            disposition: None,
            turns: turns.clone(),
        }
    }

    #[tokio::test]
    async fn platform_074_a_replica_reads_four_uploads_at_once_and_a_fifth_waits_for_a_turn() {
        let turns = UploadTurns::new();
        let workers = Workers::new();
        let mut read = Vec::new();
        for _ in 0..4 {
            read.push(upload("x", &turns).read(1, &workers).await.unwrap());
        }

        // Every turn is held while its bytes are, so a fifth upload's body waits, unread.
        let mut fifth = std::pin::pin!(upload("y", &turns).read(1, &workers));
        assert!((&mut fifth).now_or_never().is_none());
        drop(read.pop());
        let fifth = fifth.await.unwrap();

        assert_eq!(&fifth.bytes[..], b"y");
        assert_eq!(turns.0.available_permits(), 0);
        drop((read, fifth));
        assert_eq!(turns.0.available_permits(), UPLOADS_AT_ONCE);
    }
}
