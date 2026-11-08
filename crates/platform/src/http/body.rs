//! Request bodies: the JSON extractor (PLATFORM-049, PLATFORM-050), that of a JSON Merge Patch
//! (API-03), with the idempotency key of a keyed route (PLATFORM-037), the body of an upload
//! (PLATFORM-074), and the collection of every field that a request type's conversion into domain
//! types refuses (PLATFORM-051, API-10).

use std::sync::Arc;

use axum::{
    body::{Body, Bytes},
    extract::{FromRequest, Request},
    http::{HeaderValue, header},
};
use http_body_util::{BodyExt, LengthLimitError, Limited};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::docs::IDEMPOTENCY_KEY;
use crate::{
    blocking::Workers,
    errors::{BodyFault, FieldError, Internal, PlatformError, Problem, RequestError},
    idempotency::IdempotencyKey,
    types::Invalid,
};

/// A JSON body, parsed into `T`. `T` declares `#[serde(deny_unknown_fields)]`, so a field it
/// doesn't take is refused (API-08).
#[derive(Debug)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem<RequestError>;

    async fn from_request(request: Request, _: &S) -> Result<Self, Self::Rejection> {
        let bytes = read(request, JSON).await?;
        parse(&bytes).map(Self)
    }
}

/// A `PATCH` route's body, a JSON Merge Patch (RFC 7396) sent as `application/merge-patch+json`
/// (API-03), parsed into `T` as [`Json`] parses its body. `T` holds the members a patch may
/// change: one it makes optional is left as it is when the patch doesn't name it, and it may
/// require one while that is the resource's only editable member (D88).
#[derive(Debug)]
pub struct MergePatch<T>(pub T);

impl<T, S> FromRequest<S> for MergePatch<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem<RequestError>;

    async fn from_request(request: Request, _: &S) -> Result<Self, Self::Rejection> {
        let bytes = read(request, MERGE_PATCH).await?;
        parse(&bytes).map(Self)
    }
}

/// The media type of a JSON body.
const JSON: &str = "application/json";

/// The media type of a JSON Merge Patch (RFC 7396).
const MERGE_PATCH: &str = "application/merge-patch+json";

/// The media type of an upload's bytes, whose kind its route finds from the bytes themselves.
const OCTET_STREAM: &str = "application/octet-stream";

/// The JSON body of a route that accepts an `Idempotency-Key` (API-12), and the request's key, if
/// it sent one, with the fingerprint of its method, path, query and body (PLATFORM-036).
#[derive(Debug)]
pub struct Idempotent<T> {
    /// The body, parsed as [`Json`] parses it.
    pub body: T,
    /// The key, which the operation's unit of work claims.
    pub key: Option<IdempotencyKey>,
}

impl<T, S> FromRequest<S> for Idempotent<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem<RequestError>;

    async fn from_request(request: Request, _: &S) -> Result<Self, Self::Rejection> {
        let method = request.method().as_str().to_owned();
        let path = request.uri().path().to_owned();
        let query = request.uri().query().unwrap_or_default().to_owned();
        let header = request.headers().get(IDEMPOTENCY_KEY).cloned();
        let bytes = read(request, JSON).await?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let key = header
            .map(|value| {
                IdempotencyKey::parse(value.as_bytes(), &method, &path, &query, &digest)
                    .ok_or(Problem(RequestError::InvalidIdempotencyKey))
            })
            .transpose()?;
        Ok(Self {
            body: parse(&bytes)?,
            key,
        })
    }
}

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
/// method, path, query and bytes (PLATFORM-036). It holds the replica's turn until it is dropped,
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
                let key = IdempotencyKey::parse(
                    value.as_bytes(),
                    &self.method,
                    &self.path,
                    &self.query,
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

/// The body of `request`, which must be sent as `media_type`, read up to its route's limit
/// (PLATFORM-049, PLATFORM-050).
async fn read(request: Request, media_type: &'static str) -> Result<Bytes, Problem<RequestError>> {
    if !is_media_type(request.headers().get(header::CONTENT_TYPE), media_type) {
        return Err(Problem(RequestError::InvalidBody(BodyFault::ContentType(
            media_type,
        ))));
    }
    match request.into_body().collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) if is_too_large(&error) => Err(Problem(RequestError::PayloadTooLarge)),
        Err(_) => Err(Problem(RequestError::InvalidBody(BodyFault::Syntax))),
    }
}

/// `bytes` parsed into `T`, or the field the parser failed at (PLATFORM-049).
fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Problem<RequestError>> {
    let deserializer = &mut serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(deserializer).map_err(|error| {
        let fault = if error.inner().is_data() {
            BodyFault::Shape(pointer(error.path()))
        } else {
            BodyFault::Syntax
        };
        Problem(RequestError::InvalidBody(fault))
    })
}

/// Whether a `Content-Type` is `media_type`, with no parameter but a `charset` of `utf-8`.
fn is_media_type(value: Option<&axum::http::HeaderValue>, media_type: &str) -> bool {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let mut parts = value.split(';').map(str::trim);
    let media_type_ok = parts
        .next()
        .is_some_and(|sent| sent.eq_ignore_ascii_case(media_type));
    media_type_ok
        && parts.all(|parameter| {
            parameter.split_once('=').is_some_and(|(name, value)| {
                name.trim().eq_ignore_ascii_case("charset")
                    && value.trim().trim_matches('"').eq_ignore_ascii_case("utf-8")
            })
        })
}

fn is_too_large(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error.is::<LengthLimitError>() {
            return true;
        }
        current = error.source();
    }
    false
}

/// The JSON Pointer of the field a parser failed at, or `None` when it failed at the body's top.
fn pointer(path: &serde_path_to_error::Path) -> Option<String> {
    let segments: Vec<String> = path
        .iter()
        .filter_map(|segment| match segment {
            serde_path_to_error::Segment::Seq { index } => Some(index.to_string()),
            serde_path_to_error::Segment::Map { key } => Some(escape(key)),
            serde_path_to_error::Segment::Enum { variant } => Some(escape(variant)),
            serde_path_to_error::Segment::Unknown => None,
        })
        .collect();
    (!segments.is_empty()).then(|| format!("/{}", segments.join("/")))
}

/// A segment of a JSON Pointer, with `~` and `/` escaped (RFC 6901).
fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Every field a request type's conversion into domain types refused, so one response names them
/// all (API-10).
#[derive(Debug, Default)]
pub struct FieldErrors(Vec<FieldError>);

impl FieldErrors {
    /// No refused field yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of the field at the JSON Pointer `field`, or `None`, recording why, when its
    /// domain type refused it.
    pub fn check<T>(&mut self, field: &str, result: Result<T, Invalid>) -> Option<T> {
        result
            .map_err(|invalid| {
                self.0
                    .push(FieldError::new(field, invalid.code, invalid.message));
            })
            .ok()
    }

    /// `value`, which the checked fields built, unless a field was refused: then the 400
    /// `request.invalid_fields` that names every refused field (PLATFORM-051).
    pub fn finish<T>(self, value: Option<T>) -> Result<T, RequestError> {
        match value {
            Some(value) if self.0.is_empty() => Ok(value),
            _ => Err(RequestError::InvalidFields(self.0)),
        }
    }

    /// `value`, which the checked query parameters built, unless one was refused: then the 400
    /// `request.invalid_query` that names every refused parameter (PLATFORM-045).
    pub fn finish_query<T>(self, value: Option<T>) -> Result<T, RequestError> {
        match value {
            Some(value) if self.0.is_empty() => Ok(value),
            _ => Err(RequestError::InvalidQuery(self.0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;

    use super::*;

    /// Whether a request sent with `content_type` sends `media_type`.
    fn sent(content_type: Option<&'static str>, media_type: &str) -> bool {
        let header = content_type.map(axum::http::HeaderValue::from_static);
        is_media_type(header.as_ref(), media_type)
    }

    /// An upload of `body`, sharing `turns`, as the extractor makes it.
    fn upload(body: &'static str, turns: &UploadTurns) -> Upload {
        Upload {
            body: Body::from(body),
            declared: None,
            method: "POST".to_owned(),
            path: "/v1/uploads".to_owned(),
            query: String::new(),
            key: None,
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

    #[test]
    fn platform_049_a_body_must_be_sent_as_json() {
        assert!(sent(Some("application/json"), JSON));
        assert!(sent(Some("Application/JSON; charset=UTF-8"), JSON));
        assert!(!sent(None, JSON));
        assert!(!sent(Some("text/plain"), JSON));
        assert!(!sent(Some("application/json; charset=latin1"), JSON));
        assert!(!sent(Some("application/json-patch+json"), JSON));
        assert!(!sent(Some("application/merge-patch+json"), JSON));
    }

    #[test]
    fn platform_049_a_merge_patch_must_be_sent_as_one() {
        assert!(sent(Some("application/merge-patch+json"), MERGE_PATCH));
        assert!(sent(
            Some("application/merge-patch+json; charset=utf-8"),
            MERGE_PATCH
        ));
        assert!(!sent(Some("application/json"), MERGE_PATCH));
        assert!(!sent(Some("application/json-patch+json"), MERGE_PATCH));
    }

    #[test]
    fn platform_051_every_refused_field_is_named() {
        let mut errors = FieldErrors::new();
        let first = errors.check("/a", Err::<u8, _>(Invalid::new("too_long", "Too long.")));
        let second = errors.check("/b", Ok::<_, Invalid>(2));
        let third = errors.check("/c/0", Err::<u8, _>(Invalid::new("required", "Required.")));
        let error = errors.finish(first.zip(second).zip(third)).unwrap_err();
        let RequestError::InvalidFields(fields) = error else {
            unreachable!("{error:?}");
        };
        let named: Vec<(&str, &str)> = fields
            .iter()
            .map(|field| (field.field.as_str(), field.code))
            .collect();
        assert_eq!(named, [("/a", "too_long"), ("/c/0", "required")]);
    }

    #[test]
    fn platform_049_a_pointer_escapes_its_segments() {
        assert_eq!(escape("a/b~c"), "a~1b~0c");
    }
}
