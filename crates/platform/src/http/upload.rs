//! The body of an upload route: a file's bytes, read in one of the replica's turns, of which a
//! user holds one at most, with the request's idempotency key (PLATFORM-074), and counted in the
//! user's budget of uploads (PLATFORM-093).

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

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
    errors::{BodyFault, Held, Internal, PlatformError, Problem, RequestError},
    idempotency::IdempotencyKey,
    rate_limit::{Bucket, Counted, LimitError, RateLimiter, RateLimits},
    types::UserId,
};

/// The media type of an upload's bytes, whose kind its route finds from the bytes themselves.
const OCTET_STREAM: &str = "application/octet-stream";

/// How many uploads' bodies a replica holds in memory at once (PLATFORM-074).
const UPLOADS_AT_ONCE: usize = 4;

/// The budget of the bytes of each user's uploads, and its window, a day (PLATFORM-093).
const UPLOADS_BUDGET: &str = "uploads";
const UPLOADS_WINDOW: Duration = Duration::from_secs(86_400);

/// The turns of the uploads a replica reads at once, which the stack shares with every request,
/// and each user's own turn at them, so that no user holds more than one (PLATFORM-074), with
/// the budget that each user's uploads count their bytes in (PLATFORM-093).
#[derive(Clone, Debug)]
pub(super) struct UploadTurns {
    /// The replica's turns.
    all: Arc<Semaphore>,
    /// Each user's own turn, from an upload of theirs until `take` next runs while no upload of
    /// theirs holds it or waits for it.
    users: Arc<Mutex<HashMap<UserId, Arc<Semaphore>>>>,
    /// The budget of one user's uploads.
    budget: Bucket,
}

/// A turn at reading an upload's body: its user's own, and one of the replica's, which both go
/// when it is dropped (PLATFORM-074).
#[derive(Debug)]
struct Turn {
    _own: OwnedSemaphorePermit,
    _shared: OwnedSemaphorePermit,
}

impl UploadTurns {
    /// Every turn free, and a budget of `bytes` a day for each user's uploads.
    pub(super) fn new(bytes: u32) -> Self {
        Self {
            all: Arc::new(Semaphore::new(UPLOADS_AT_ONCE)),
            users: Arc::default(),
            budget: Bucket {
                name: UPLOADS_BUDGET,
                limit: bytes,
                window: UPLOADS_WINDOW,
            },
        }
    }

    /// Waits for `caller`'s own turn, which their earlier upload holds until its bytes are
    /// dropped, and only then for one of the replica's, so a user's uploads hold one of the
    /// replica's turns between them (PLATFORM-074).
    async fn take(&self, caller: UserId) -> Result<Turn, Internal> {
        let own = {
            // A lock that a panic poisoned still holds a whole map, since each change of it is
            // one call, so it is taken as it is.
            let mut users = self.users.lock().held();
            // A user's turn that no upload holds or waits for is only the map's, and goes here.
            users.retain(|_, own| Arc::strong_count(own) > 1);
            Arc::clone(
                users
                    .entry(caller)
                    .or_insert_with(|| Arc::new(Semaphore::new(1))),
            )
        };
        // Neither semaphore is ever closed, so waiting for a turn ends only with one.
        let own = own.acquire_owned().await.map_err(Internal::new)?;
        let shared = Arc::clone(&self.all)
            .acquire_owned()
            .await
            .map_err(Internal::new)?;
        Ok(Turn {
            _own: own,
            _shared: shared,
        })
    }
}

/// The body of an upload route: a file's bytes, sent as `application/octet-stream`, a body that
/// isn't JSON (API-08), which the handler reads once it has refused what it refuses without them,
/// such as a caller who may not upload (PLATFORM-073), up to the limit its module sets, in one of
/// the replica's turns, with the request's `Idempotency-Key`, if it is a `POST` that sent one
/// (PLATFORM-074). An upload route's group sets no body limit of its own, so nothing reads the
/// body before the handler does.
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
    /// The request's rate limits, with which [`Upload::read`] counts the body's bytes: every
    /// request that the pipeline's IP limit counted has them (PLATFORM-054).
    limits: Option<RateLimits>,
}

/// Prints neither the file's name, which `Content-Disposition` gives, nor its bytes: both are
/// personal data (OBS-04).
impl fmt::Debug for Upload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Upload")
            .field("declared", &self.declared)
            .field("method", &self.method)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl<S: Send + Sync> FromRequest<S> for Upload {
    type Rejection = Problem<PlatformError>;

    async fn from_request(request: Request, _: &S) -> Result<Self, Self::Rejection> {
        let Some(turns) = request.extensions().get::<UploadTurns>().cloned() else {
            return Err(Problem(
                Internal::new("the request carries no upload turns").into(),
            ));
        };
        // Only a `POST` takes a key (API-12): an upload by another method, such as setting an
        // avatar by `PUT`, reads none, as a route that takes no key reads none.
        let key = if request.method() == axum::http::Method::POST {
            request.headers().get(IDEMPOTENCY_KEY).cloned()
        } else {
            None
        };
        // A key that isn't one is refused before the body's media type (API-10).
        if key
            .as_ref()
            .is_some_and(|value| !IdempotencyKey::is_key(value.as_bytes()))
        {
            return Err(Problem(RequestError::InvalidIdempotencyKey.into()));
        }
        if !is_media_type(request.headers().get(header::CONTENT_TYPE), OCTET_STREAM) {
            return Err(Problem(
                RequestError::InvalidBody(BodyFault::ContentType(OCTET_STREAM)).into(),
            ));
        }
        let declared = request.headers().get(header::CONTENT_LENGTH).map(|length| {
            length
                .to_str()
                .ok()
                .and_then(|length| length.parse::<usize>().ok())
        });
        let limiter = request.extensions().get::<RateLimiter>().cloned();
        let counted = request.extensions().get::<Counted>().cloned();
        Ok(Self {
            limits: limiter
                .zip(counted)
                .map(|(limiter, counted)| RateLimits::new(limiter, counted)),
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
pub struct Uploaded {
    /// The bytes.
    pub bytes: Bytes,
    /// The key, which the operation's unit of work claims.
    pub key: Option<IdempotencyKey>,
    _turn: Turn,
}

/// Prints how many bytes the file holds, and not the bytes, which are personal data (OBS-04).
impl fmt::Debug for Uploaded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Uploaded")
            .field("length", &self.bytes.len())
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
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

    /// Waits for the turn of `caller`, the user who uploads, and for one of the replica's, then
    /// reads the body, up to `limit` bytes: 413 `request.payload_too_large` for a body that
    /// declares more or grows past it (PLATFORM-048, PLATFORM-050), and 400
    /// `request.invalid_body` for one that stops before its end. It then counts the bytes it
    /// read in the caller's budget of uploads, among the request's rate limits: 429
    /// `request.rate_limited` once they take the budget's window beyond its limit
    /// (PLATFORM-093). A keyed upload's digest of the bytes, for its key's fingerprint, is
    /// CPU-heavy work, which runs through `workers` (CODE-05).
    pub async fn read(
        mut self,
        caller: UserId,
        limit: usize,
        workers: &Workers,
    ) -> Result<Uploaded, PlatformError> {
        let Some(limits) = self.limits.take() else {
            return Err(Internal::new("the upload's request passed no rate limit").into());
        };
        let mut uploaded = self.held(caller, limit).await?;
        let spent = u32::try_from(uploaded.bytes.len()).unwrap_or(u32::MAX);
        let budget = self.turns.budget;
        match limits.spend(budget, caller.uuid().as_bytes(), spent).await {
            Ok(()) => {}
            Err(LimitError::RateLimited) => return Err(RequestError::RateLimited.into()),
            Err(LimitError::Internal(error)) => return Err(error.into()),
        }
        if let Some(value) = &self.key {
            let digested = uploaded.bytes.clone();
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
            uploaded.key = Some(key);
        }
        Ok(uploaded)
    }

    /// Waits for `caller`'s turn, and for one of the replica's, then reads the body, up to
    /// `limit` bytes, into the bytes that hold the turn, with no key yet (PLATFORM-074).
    async fn held(&mut self, caller: UserId, limit: usize) -> Result<Uploaded, PlatformError> {
        self.refuse_declared(limit)?;
        let turn = self.turns.take(caller).await?;
        let body = std::mem::take(&mut self.body);
        let bytes = match Limited::new(body, limit).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(error) if is_too_large(&*error) => return Err(RequestError::PayloadTooLarge.into()),
            Err(_) => return Err(RequestError::InvalidBody(BodyFault::Incomplete).into()),
        };
        Ok(Uploaded {
            bytes,
            key: None,
            _turn: turn,
        })
    }
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;

    use super::*;

    /// The turns of a replica, with a budget that no test here counts in.
    fn turns() -> UploadTurns {
        UploadTurns::new(1)
    }

    /// Reads `upload`'s one byte in `caller`'s turn, as [`Upload::read`] does before it counts
    /// the byte.
    async fn held(mut upload: Upload, caller: UserId) -> Uploaded {
        upload.held(caller, 1).await.unwrap()
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
            disposition: None,
            turns: turns.clone(),
            limits: None,
        }
    }

    #[tokio::test]
    async fn platform_074_an_upload_by_another_method_than_post_reads_no_key() {
        use crate::errors::ErrorCode;

        let turns = turns();
        for method in ["PUT", "POST"] {
            let request = Request::builder()
                .method(method)
                .uri("/v1/uploads")
                .header(header::CONTENT_TYPE, OCTET_STREAM)
                .header(IDEMPOTENCY_KEY, "not a key")
                .extension(turns.clone())
                .body(Body::from("x"))
                .unwrap();

            let upload = Upload::from_request(request, &()).await;

            match method {
                "PUT" => assert!(upload.unwrap().key.is_none()),
                _ => assert_eq!(
                    upload.unwrap_err().0.code(),
                    "request.invalid_idempotency_key"
                ),
            }
        }
    }

    #[tokio::test]
    async fn api_10_an_upload_refuses_a_key_that_is_not_one_before_the_body() {
        use crate::errors::ErrorCode;

        let request = Request::builder()
            .method("POST")
            .uri("/v1/uploads")
            .header(header::CONTENT_TYPE, "text/plain")
            .header(IDEMPOTENCY_KEY, "not a key")
            .extension(turns())
            .body(Body::from("x"))
            .unwrap();

        let refused = Upload::from_request(request, &()).await;

        assert_eq!(
            refused.unwrap_err().0.code(),
            "request.invalid_idempotency_key"
        );
    }

    /// The user numbered `number`.
    fn user(number: u128) -> UserId {
        UserId::from_uuid(uuid::Uuid::from_u128(number))
    }

    #[tokio::test]
    async fn obs_04_an_upload_prints_neither_its_file_s_name_nor_its_bytes() {
        let mut upload = upload("the file's bytes", &turns());
        upload.disposition = Some(HeaderValue::from_static(
            "attachment; filename=\"Ada's notes.pdf\"",
        ));

        let unread = format!("{upload:?}");
        let read = format!("{:?}", upload.held(user(1), 64).await.unwrap());

        for printed in [unread, read] {
            assert!(!printed.contains("notes"), "{printed}");
            assert!(!printed.contains("the file"), "{printed}");
        }
    }

    #[tokio::test]
    async fn platform_074_a_replica_reads_four_uploads_at_once_and_a_fifth_waits_for_a_turn() {
        let turns = turns();
        let mut read = Vec::new();
        for number in 0..4 {
            read.push(held(upload("x", &turns), user(number)).await);
        }

        // Every turn is held while its bytes are, so a fifth user's upload waits, unread.
        let mut fifth = std::pin::pin!(held(upload("y", &turns), user(4)));
        assert!((&mut fifth).now_or_never().is_none());
        drop(read.pop());
        let fifth = fifth.await;

        assert_eq!(&fifth.bytes[..], b"y");
        assert_eq!(turns.all.available_permits(), 0);
        drop((read, fifth));
        assert_eq!(turns.all.available_permits(), UPLOADS_AT_ONCE);
    }

    #[tokio::test]
    async fn platform_074_a_user_s_next_upload_waits_for_their_first_holding_no_turn() {
        let turns = turns();
        let (ada, bob) = (user(1), user(2));
        let first = held(upload("x", &turns), ada).await;

        // Ada's next upload waits for her first, and takes none of the replica's turns
        // meanwhile, so another user's upload is read.
        let mut next = std::pin::pin!(held(upload("y", &turns), ada));
        assert!((&mut next).now_or_never().is_none());
        assert_eq!(turns.all.available_permits(), UPLOADS_AT_ONCE - 1);
        let others = held(upload("z", &turns), bob).await;
        assert!((&mut next).now_or_never().is_none());
        drop(first);
        let next = next.await;

        assert_eq!(&next.bytes[..], b"y");
        assert_eq!(turns.all.available_permits(), UPLOADS_AT_ONCE - 2);
        drop((next, others));
        assert_eq!(turns.all.available_permits(), UPLOADS_AT_ONCE);
        // The next upload drops each own turn that no upload holds or waits for, so only its own
        // user's remains.
        let last = held(upload("w", &turns), bob).await;
        let users: Vec<UserId> = turns.users.lock().unwrap().keys().copied().collect();
        assert_eq!(users, [bob]);
        drop(last);
    }
}
