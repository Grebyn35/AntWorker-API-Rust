//! The body of a route that takes a batch of telemetry, as a client's exporter sends it over the
//! OpenTelemetry Protocol on HTTP (OTLP/HTTP), and the response that tells the exporter that the
//! batch was taken (PLATFORM-090).

use std::{io::Read, sync::Arc};

use axum::{
    body::{Body, Bytes},
    extract::{FromRequest, Request},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use flate2::read::MultiGzDecoder;
use http_body_util::{BodyExt, Limited};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use utoipa_axum::router::UtoipaMethodRouter;

use super::{
    Access, Routes,
    body::{is_media_type, is_too_large},
    stack,
};
use crate::{
    blocking::Workers,
    errors::{BodyFault, ErrorCode, Internal, PlatformError, Problem, RequestError},
};

impl<S> Routes<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// Adds a route that a signed-in user calls, as [`Routes::user`] does, whose body is a batch
    /// of telemetry, which its handler reads through [`Batch`], and which can answer `errors`
    /// besides the pipeline's own (ERR-13). Its group sets no body limit of its own
    /// (PLATFORM-090).
    pub fn user_batch(self, route: UtoipaMethodRouter<S>, errors: &[&dyn ErrorCode]) -> Self {
        self.add_with(Access::User, route, errors, stack::batch_group)
    }
}

/// What a refusal of another `Content-Type` names: the two media types of the protocol.
const MEDIA_TYPES: &str = "application/x-protobuf or application/json";

/// How many batches a replica holds in memory at once (PLATFORM-090).
const BATCHES_AT_ONCE: usize = 8;

/// The turns of the batches that a replica reads at once, which the stack shares with every
/// request (PLATFORM-090).
#[derive(Clone, Debug)]
pub(super) struct BatchTurns(Arc<Semaphore>);

impl BatchTurns {
    /// Every turn free.
    pub(super) fn new() -> Self {
        Self(Arc::new(Semaphore::new(BATCHES_AT_ONCE)))
    }
}

/// How the protocol encodes a batch, which its `Content-Type` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatchEncoding {
    /// The protocol's messages as binary Protobuf.
    Protobuf,
    /// The protocol's messages as JSON.
    Json,
}

impl BatchEncoding {
    /// The media type of a body in this encoding.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Protobuf => "application/x-protobuf",
            Self::Json => "application/json",
        }
    }
}

/// The body of a route that takes a batch of telemetry, sent as `application/x-protobuf` or as
/// `application/json`, with `Content-Encoding: gzip` or without one (PLATFORM-090). Extracting
/// it reads nothing of the body, and such a route's group sets no body limit of its own: the
/// handler's [`Batch::read`] reads the body, in one of the replica's turns, up to the limit that
/// its module sets.
#[derive(Debug)]
pub struct Batch {
    body: Body,
    turns: BatchTurns,
    /// The body's declared length: `None` without `Content-Length`, and `Some(None)` for one that
    /// isn't a number Rust can hold.
    declared: Option<Option<usize>>,
    content_type: Option<HeaderValue>,
    content_encoding: Option<HeaderValue>,
}

impl<S: Send + Sync> FromRequest<S> for Batch {
    type Rejection = Problem<Internal>;

    async fn from_request(request: Request, _: &S) -> Result<Self, Self::Rejection> {
        let Some(turns) = request.extensions().get::<BatchTurns>().cloned() else {
            return Err(Problem(Internal::new("the request carries no batch turns")));
        };
        let declared = request.headers().get(header::CONTENT_LENGTH).map(|length| {
            length
                .to_str()
                .ok()
                .and_then(|length| length.parse::<usize>().ok())
        });
        Ok(Self {
            declared,
            turns,
            content_type: request.headers().get(header::CONTENT_TYPE).cloned(),
            content_encoding: request.headers().get(header::CONTENT_ENCODING).cloned(),
            body: request.into_body(),
        })
    }
}

/// Whether `value`, a `Content-Encoding`, names the one coding `coding`, in any case.
fn is_coding(value: &HeaderValue, coding: &str) -> bool {
    value
        .to_str()
        .is_ok_and(|value| value.trim().eq_ignore_ascii_case(coding))
}

/// A batch that [`Batch::read`] read. It holds the replica's turn until it is dropped, so the
/// handler keeps it until it has done with the bytes (PLATFORM-090).
#[derive(Debug)]
pub struct ReadBatch {
    /// How the batch is encoded.
    pub encoding: BatchEncoding,
    /// Its bytes, inflated if they came as gzip.
    pub bytes: Bytes,
    _turn: OwnedSemaphorePermit,
}

impl Batch {
    /// How the batch is encoded, which its `Content-Type` says: 400 `request.invalid_body` for
    /// another media type than the protocol's two.
    fn encoding(&self) -> Result<BatchEncoding, RequestError> {
        [BatchEncoding::Protobuf, BatchEncoding::Json]
            .into_iter()
            .find(|encoding| is_media_type(self.content_type.as_ref(), encoding.media_type()))
            .ok_or(RequestError::InvalidBody(BodyFault::ContentType(
                MEDIA_TYPES,
            )))
    }

    /// Whether the batch came as gzip, which its `Content-Encoding` says: 400
    /// `request.invalid_body` for another coding than `gzip` or `identity`.
    fn is_gzip(&self) -> Result<bool, RequestError> {
        match &self.content_encoding {
            None => Ok(false),
            Some(coding) if is_coding(coding, "identity") => Ok(false),
            Some(coding) if is_coding(coding, "gzip") => Ok(true),
            Some(_) => Err(RequestError::InvalidBody(BodyFault::ContentEncoding)),
        }
    }

    /// Waits for one of the replica's turns, then reads the body, up to `limit` bytes, and
    /// inflates one that came as gzip, up to as many: 400 `request.invalid_body` for another
    /// `Content-Type` or `Content-Encoding` than a batch's, for a body that stops before its end,
    /// and for gzip that doesn't inflate, and 413 `request.payload_too_large` for a body that
    /// declares more than `limit` bytes, grows past them, or inflates past them. Inflating is
    /// CPU-heavy work, which runs through `workers` (CODE-05), and stops at the first byte past
    /// `limit`.
    pub async fn read(self, limit: usize, workers: &Workers) -> Result<ReadBatch, PlatformError> {
        let encoding = self.encoding()?;
        let gzip = self.is_gzip()?;
        // A length that isn't a number Rust can hold is over any limit.
        if self
            .declared
            .is_some_and(|length| length.is_none_or(|length| length > limit))
        {
            return Err(RequestError::PayloadTooLarge.into());
        }
        // The semaphore is never closed, so waiting for a turn ends only with one.
        let turn = self.turns.0.acquire_owned().await.map_err(Internal::new)?;
        let sent = match Limited::new(self.body, limit).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(error) if is_too_large(&*error) => return Err(RequestError::PayloadTooLarge.into()),
            Err(_) => return Err(RequestError::InvalidBody(BodyFault::Incomplete).into()),
        };
        let bytes = if gzip {
            workers
                .run("batch inflation", move || inflate(&sent, limit))
                .await
                .map_err(Internal::new)??
        } else {
            sent
        };
        Ok(ReadBatch {
            encoding,
            bytes,
            _turn: turn,
        })
    }
}

/// The bytes that `gzip` inflates to, of every member it holds, when they are at most `limit`.
fn inflate(gzip: &[u8], limit: usize) -> Result<Bytes, RequestError> {
    let most = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut inflated = Vec::new();
    match MultiGzDecoder::new(gzip)
        .take(most)
        .read_to_end(&mut inflated)
    {
        Ok(_) if inflated.len() > limit => Err(RequestError::PayloadTooLarge),
        Ok(_) => Ok(inflated.into()),
        Err(_) => Err(RequestError::InvalidBody(BodyFault::Gzip)),
    }
}

/// A batch as the protocol's JSON, which the document describes as an object and no further,
/// since the protocol defines its members (API-17).
#[derive(utoipa::ToSchema)]
#[schema(value_type = Object)]
pub struct BatchJson(pub serde_json::Value);

/// The protocol's full success as JSON, an object without members (API-17).
#[derive(utoipa::ToSchema)]
#[schema(value_type = Object)]
pub struct BatchTakenJson(pub serde_json::Value);

/// The response to a batch that was taken: 200 with the protocol's full success in the batch's
/// encoding, which holds no bytes as Protobuf and is `{}` as JSON (PLATFORM-090).
#[derive(Clone, Copy, Debug)]
pub struct BatchTaken(pub BatchEncoding);

impl IntoResponse for BatchTaken {
    fn into_response(self) -> Response {
        let body = match self.0 {
            BatchEncoding::Protobuf => "",
            BatchEncoding::Json => "{}",
        };
        (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(self.0.media_type()),
            )],
            body,
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};

    use super::*;

    /// `bytes` as one member of gzip.
    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// What reading a request with `headers` and `body` gives, up to `limit` bytes.
    async fn read(
        headers: &[(&str, &str)],
        body: Vec<u8>,
        limit: usize,
    ) -> Result<ReadBatch, PlatformError> {
        let mut request = Request::builder().method("POST").uri("/v1/batches");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request
            .extension(BatchTurns::new())
            .body(Body::from(body))
            .unwrap();
        let batch = Batch::from_request(request, &()).await.unwrap();
        batch.read(limit, &Workers::new()).await
    }

    const PROTOBUF: (&str, &str) = ("content-type", "application/x-protobuf");
    const GZIP: (&str, &str) = ("content-encoding", "gzip");

    #[tokio::test]
    async fn platform_090_a_batch_is_protobuf_or_json_and_gzip_or_not() {
        let protobuf = read(&[PROTOBUF], vec![1, 2, 3], 3).await.unwrap();
        let json = read(
            &[("content-type", "application/json; charset=utf-8")],
            b"{}".to_vec(),
            3,
        )
        .await
        .unwrap();
        let identity = read(&[PROTOBUF, ("content-encoding", "identity")], vec![4], 3)
            .await
            .unwrap();
        let gzipped = read(&[PROTOBUF, ("content-encoding", "GZIP")], gzip(&[5]), 30)
            .await
            .unwrap();

        assert_eq!(protobuf.encoding, BatchEncoding::Protobuf);
        assert_eq!(&protobuf.bytes[..], [1, 2, 3]);
        assert_eq!(json.encoding, BatchEncoding::Json);
        assert_eq!(&json.bytes[..], b"{}");
        assert_eq!(&identity.bytes[..], [4]);
        assert_eq!(&gzipped.bytes[..], [5]);
    }

    #[tokio::test]
    async fn platform_090_another_content_type_or_content_encoding_is_refused() {
        for headers in [
            &[][..],
            &[("content-type", "text/plain")],
            &[("content-type", "application/octet-stream")],
            &[PROTOBUF, ("content-encoding", "br")],
            &[PROTOBUF, ("content-encoding", "gzip, gzip")],
            &[PROTOBUF, ("content-encoding", "")],
        ] {
            let refusal = read(headers, vec![], 10).await.unwrap_err();

            assert_eq!(refusal.code(), "request.invalid_body", "{headers:?}");
        }
    }

    #[tokio::test]
    async fn platform_090_a_gzip_body_is_inflated_whatever_members_it_holds() {
        let mut members = gzip(b"spans ");
        members.extend(gzip(b"and more spans"));

        let inflated = read(&[PROTOBUF, GZIP], members, 100).await.unwrap();

        assert_eq!(&inflated.bytes[..], b"spans and more spans");
    }

    #[tokio::test]
    async fn platform_090_a_body_over_the_limit_as_sent_or_inflated_is_too_large() {
        // A kilobyte of zeros, which gzip holds in fewer than 40 bytes.
        let bomb = gzip(&[0; 1024]);
        assert!(bomb.len() < 40);

        let sent = read(&[PROTOBUF], vec![0; 11], 10).await;
        let declared = read(&[PROTOBUF, ("content-length", "11")], vec![], 10).await;
        let inflated = read(&[PROTOBUF, GZIP], bomb.clone(), 1023).await;
        let fits = read(&[PROTOBUF, GZIP], bomb, 1024).await;

        for refused in [sent, declared, inflated] {
            assert_eq!(refused.unwrap_err().code(), "request.payload_too_large");
        }
        assert_eq!(fits.unwrap().bytes.len(), 1024);
    }

    #[tokio::test]
    async fn platform_090_gzip_that_does_not_inflate_is_an_invalid_body() {
        let whole = gzip(b"a batch of spans");
        let cut = whole[..whole.len() - 4].to_vec();
        let mut trailed = whole;
        trailed.extend(b"not gzip");

        for body in [b"not gzip".to_vec(), cut, trailed] {
            let refusal = read(&[PROTOBUF, GZIP], body, 1024).await.unwrap_err();

            assert_eq!(refusal.code(), "request.invalid_body");
        }
    }

    #[tokio::test]
    async fn platform_090_a_replica_reads_eight_batches_at_once_and_a_ninth_waits_for_a_turn() {
        use futures_util::FutureExt;

        let (turns, workers) = (BatchTurns::new(), Workers::new());
        let batch = || async {
            let request = Request::builder()
                .header(PROTOBUF.0, PROTOBUF.1)
                .extension(turns.clone())
                .body(Body::from("x"))
                .unwrap();
            Batch::from_request(request, &()).await.unwrap()
        };
        let mut read = Vec::new();
        for _ in 0..8 {
            read.push(batch().await.read(1, &workers).await.unwrap());
        }

        // Every turn is held while its bytes are, so a ninth batch's body waits, unread.
        let ninth = batch().await;
        let mut ninth = std::pin::pin!(ninth.read(1, &workers));
        assert!((&mut ninth).now_or_never().is_none());
        drop(read.pop());
        let ninth = ninth.await.unwrap();

        assert_eq!(&ninth.bytes[..], b"x");
        assert_eq!(turns.0.available_permits(), 0);
        drop((read, ninth));
        assert_eq!(turns.0.available_permits(), BATCHES_AT_ONCE);
    }

    #[tokio::test]
    async fn platform_090_a_taken_batch_answers_the_full_success_in_its_encoding() {
        for (encoding, media_type, body) in [
            (BatchEncoding::Protobuf, "application/x-protobuf", ""),
            (BatchEncoding::Json, "application/json", "{}"),
        ] {
            let response = BatchTaken(encoding).into_response();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], media_type);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(&bytes[..], body.as_bytes());
        }
    }
}
