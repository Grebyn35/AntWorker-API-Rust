//! The client of the S3-compatible bucket that holds files' bytes (D1, D14, the files spec's
//! "Storage"). It signs each request as a URL, with `rusty-s3`, and sends it through a `reqwest`
//! client of its own. It reports a bucket that doesn't answer as unavailable, and every other
//! failure as internal, neither naming the key, so no error of a library leaves it (ERR-04).
//! What the bucket keeps of an object that was deleted or replaced is in `versions.rs`.

mod failures;
mod key;
mod listing;
mod reads;
mod versions;

use std::{path::PathBuf, sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::{
    StreamExt,
    future::{Either, select},
    stream::{self, BoxStream},
};
use reqwest::header::CONTENT_TYPE;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use tracing::Instrument;
use url::Url;

use self::failures::answered_with;
pub use self::key::ObjectKey;
pub use self::versions::{Content, Version, VersionBytes};
use crate::{
    clock::SharedClock,
    errors::Internal,
    telemetry::{metrics::Counter, names},
    types::Secret,
};

/// How long connecting to the bucket may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long the bucket has to answer a read, a deletion or a page of a listing, and to send each
/// next chunk of an object's bytes once they are asked for, before it counts as not answering:
/// short enough that a download, whose route has 10 seconds, answers 503 while the bucket doesn't
/// answer (FILES-012).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(4);

/// How long the bucket has to take a write and answer it: its share of the upload route's 60
/// seconds, after the body and its scan (PLATFORM-074, the files spec's "Scanning").
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the bucket takes a request's signed URL for, from its signing: longer than the
/// longest wait for an answer, so no request that the client still waits on is signed too long
/// ago.
const URL_LIFETIME: Duration = Duration::from_secs(60);

static REQUESTS: Counter<1> = Counter::new(
    "platform.storage_requests",
    "{request}",
    "Calls to the bucket, a write, a read, a deletion or a listing each, by how each ended: ok, \
     not_found, failed, when the bucket refused it or answered what the client couldn't read, or \
     unavailable, when it didn't answer (FILES-012). A read whose bytes then fail or stop counts \
     again, by that failure.",
    [names::STORAGE_OUTCOME],
);

/// What reaching the bucket takes (PLATFORM-076).
#[derive(Clone, Debug)]
pub struct StorageConfig {
    /// Its endpoint, such as `https://s3.eu-central-003.backblazeb2.com`.
    pub endpoint: Url,
    /// Its region.
    pub region: String,
    /// Its name.
    pub bucket: String,
    /// The ID of the key pair that reaches it.
    pub access_key_id: String,
    /// The secret of that key pair.
    pub secret_access_key: Secret<String>,
    /// Whether the bucket's name goes in the path rather than the host, for a provider whose
    /// bucket names don't appear in its host names.
    pub path_style: bool,
    /// A PEM file of the certificates that the bucket's certificate may also chain to, beside the
    /// system's, such as a private authority's, for a bucket of the operator's own.
    pub ca_path: Option<PathBuf>,
}

/// The bucket.
#[derive(Clone)]
pub struct Storage {
    client: reqwest::Client,
    bucket: Arc<Bucket>,
    credentials: Arc<Credentials>,
    clock: SharedClock,
    host: Arc<str>,
    port: u16,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Storage")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// A failure to reach the bucket.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The bucket holds no object of the key.
    #[error("the bucket holds no object of the key")]
    NotFound,
    /// The bucket didn't answer in time, the connection to it failed, or it answered that it
    /// can't serve now, with a 5xx or a 429 (FILES-012).
    #[error("the bucket didn't answer")]
    Unavailable(#[source] Internal),
    /// The bucket refused the request, or answered what the client couldn't read.
    #[error("the bucket's request failed")]
    Failed(#[source] Internal),
}

/// An object that a listing gives: its key, and when the bucket wrote it, by the bucket's clock.
#[derive(Clone, Debug)]
pub struct Listed {
    /// Its key.
    pub key: ObjectKey,
    /// When the bucket wrote it.
    pub written_at: time::OffsetDateTime,
}

/// An object's bytes, a chunk at a time, as the bucket streams them. A failure while they stream
/// ends the stream.
pub type ObjectBytes = BoxStream<'static, Result<Bytes, Internal>>;

/// An object's bytes, or the range of them that a read asked for, as the bucket streams them,
/// with the object's whole size.
pub struct Object {
    /// How many bytes the whole object holds, whatever range the read asked for.
    pub size: u64,
    /// Its bytes, or those of the range.
    pub bytes: ObjectBytes,
}

/// A call to the bucket, which names its span and its failures' causes.
#[derive(Clone, Copy, Debug)]
enum Call {
    Put,
    Get,
    Head,
    Delete,
    List,
    Versions,
}

impl Call {
    /// The S3 operation, which names the call's span.
    fn operation(self) -> &'static str {
        match self {
            Self::Put => "S3 PutObject",
            Self::Get => "S3 GetObject",
            Self::Head => "S3 HeadObject",
            Self::Delete => "S3 DeleteObject",
            Self::List => "S3 ListObjectsV2",
            Self::Versions => "S3 ListObjectVersions",
        }
    }

    /// How long the bucket has to answer it.
    fn timeout(self) -> Duration {
        match self {
            Self::Put => WRITE_TIMEOUT,
            Self::Get | Self::Head | Self::Delete | Self::List | Self::Versions => ANSWER_TIMEOUT,
        }
    }
}

impl Storage {
    /// The client of the bucket that `config` describes, whose waits for the bucket's answers are
    /// measured by `clock`. It reaches the bucket only when asked, so a bucket that doesn't
    /// answer at startup keeps no route from serving. Building it reads the certificates it
    /// trusts from their files, once (the platform spec's Storage).
    pub fn new(config: &StorageConfig, clock: SharedClock) -> Result<Self, Internal> {
        let style = if config.path_style {
            UrlStyle::Path
        } else {
            UrlStyle::VirtualHost
        };
        let bucket = Bucket::new(
            config.endpoint.clone(),
            style,
            config.bucket.clone(),
            config.region.clone(),
        )
        .map_err(|error| Internal::new(format!("the bucket's address: {error}")))?;
        let credentials = Credentials::new(
            config.access_key_id.clone(),
            config.secret_access_key.expose().clone(),
        );
        let host = config
            .endpoint
            .host_str()
            .ok_or_else(|| Internal::new("the bucket's endpoint has no host"))?;
        Ok(Self {
            client: client(config.ca_path.as_deref())?,
            bucket: Arc::new(bucket),
            credentials: Arc::new(credentials),
            clock,
            host: host.into(),
            port: config.endpoint.port_or_known_default().unwrap_or(443),
        })
    }

    /// Writes `bytes` as the object `key`, of media type `media_type`.
    pub async fn put(
        &self,
        key: &ObjectKey,
        bytes: Bytes,
        media_type: &str,
    ) -> Result<(), StorageError> {
        let request = key.addressed().map(|object| {
            self.client
                .put(writing(&self.bucket, &self.credentials, object, media_type))
                .header(CONTENT_TYPE, media_type)
                .body(bytes)
        });
        self.answer(Call::Put, request).await.map(drop)
    }

    /// Deletes the object `key`. An object that isn't there counts as deleted, so a second
    /// deletion of it succeeds.
    pub async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        let request = key.addressed().map(|object| {
            let action = self.bucket.delete_object(Some(&self.credentials), object);
            self.client.delete(action.sign(URL_LIFETIME))
        });
        match self.answer(Call::Delete, request).await {
            Ok(_) | Err(StorageError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The objects whose keys start with `prefix`, as the bucket lists them, a page at a time.
    pub fn list(&self, prefix: &str) -> BoxStream<'static, Result<Listed, StorageError>> {
        self.listed(listing::pages(self.clone(), prefix.to_owned()), Call::List)
    }

    /// What `pages`, those of the listing `call`, list, in the listing's client span (OBS-06).
    /// The bucket has as long for each page as for any other answer, and the listing counts
    /// once, as it ends or fails.
    fn listed<T: Send + 'static>(
        &self,
        pages: BoxStream<'static, Result<Vec<T>, StorageError>>,
        call: Call,
    ) -> BoxStream<'static, Result<T, StorageError>> {
        let items = answered(pages, self.clock.clone(), call)
            .flat_map(|page| match page {
                Ok(listed) => stream::iter(listed.into_iter().map(Ok)).left_stream(),
                Err(error) => stream::iter([Err(error)]).right_stream(),
            })
            .boxed();
        stream::unfold(Some((items, self.span(call))), |state| async move {
            let (mut items, span) = state?;
            match items.next().instrument(span.clone()).await {
                None => {
                    count(None);
                    None
                }
                Some(Ok(item)) => Some((Ok(item), Some((items, span)))),
                Some(Err(error)) => {
                    count(Some(&error));
                    Some((Err(error), None))
                }
            }
        })
        .boxed()
    }

    /// The bucket's answer to `request`, as [`Storage::answer_reading`] waits for it and counts
    /// it, of which the call reads nothing more.
    async fn answer(
        &self,
        call: Call,
        request: Result<reqwest::RequestBuilder, StorageError>,
    ) -> Result<reqwest::Response, StorageError> {
        self.answer_reading(call, request, |_| Ok(()))
            .await
            .map(|(found, ())| found)
    }

    /// Sends `request` for `call`, unless building it already failed, and waits for the bucket's
    /// answer, in the call's client span (OBS-06), for at most the call's timeout on the clock,
    /// then gives it with what `read` reads of it, such as the size of an object, and counts how
    /// the call ended, once: an answer whose status isn't a success, or that `read` fails on, is
    /// a failure.
    async fn answer_reading<T>(
        &self,
        call: Call,
        request: Result<reqwest::RequestBuilder, StorageError>,
        read: impl FnOnce(&reqwest::Response) -> Result<T, StorageError>,
    ) -> Result<(reqwest::Response, T), StorageError> {
        let wait = self.clock.sleep(call.timeout());
        let answered = async {
            let answering = async { answered_with(request?.send().await, call) };
            match select(std::pin::pin!(answering), wait).await {
                Either::Left((answer, _)) => answer,
                Either::Right(((), _)) => Err(StorageError::Unavailable(Internal::new(format!(
                    "the bucket didn't answer a {} within {} seconds",
                    call.operation(),
                    call.timeout().as_secs()
                )))),
            }
        }
        .instrument(self.span(call))
        .await;
        let read = answered.and_then(|found| {
            let value = read(&found)?;
            Ok((found, value))
        });
        count(read.as_ref().map(drop).err());
        read
    }

    /// The client span of `call` (OBS-06), which names the bucket's address and never the key.
    fn span(&self, call: Call) -> tracing::Span {
        tracing::info_span!(
            "s3",
            { names::OTEL_NAME } = call.operation(),
            { names::OTEL_KIND } = "client",
            { names::SERVER_ADDRESS } = &*self.host,
            { names::SERVER_PORT } = i64::from(self.port),
        )
    }
}

impl StorageError {
    /// The cause of the failure, for the logs, which names no key.
    fn into_cause(self) -> Internal {
        match self {
            Self::NotFound => Internal::new("the bucket holds no object of the key it streamed"),
            Self::Unavailable(cause) | Self::Failed(cause) => cause,
        }
    }
}

/// The signed URL of a write of `object` to `bucket`, under `credentials`, which signs
/// `media_type` as the request's `Content-Type`, so the bucket stores the one that the request
/// sends.
fn writing(bucket: &Bucket, credentials: &Credentials, object: &str, media_type: &str) -> Url {
    let mut action = bucket.put_object(Some(credentials), object);
    action.headers_mut().insert("content-type", media_type);
    action.sign(URL_LIFETIME)
}

/// Counts a call to the bucket that ended with `failure`, or with none.
fn count(failure: Option<&StorageError>) {
    let outcome = match failure {
        None => "ok",
        Some(StorageError::NotFound) => "not_found",
        Some(StorageError::Failed(_)) => "failed",
        Some(StorageError::Unavailable(_)) => "unavailable",
    };
    REQUESTS.add(1, [outcome.into()]);
}

/// `items`, the answers of `call`, each of which the bucket has `ANSWER_TIMEOUT` on `clock` to
/// give, counted from when it is asked for, so a stream that waits on its reader isn't cut short.
/// The stream ends after its first failure.
fn answered<T: Send + 'static>(
    items: impl futures_util::Stream<Item = Result<T, StorageError>> + Send + 'static,
    clock: SharedClock,
    call: Call,
) -> BoxStream<'static, Result<T, StorageError>> {
    stream::unfold(Some(items.boxed()), move |items| {
        let clock = clock.clone();
        async move {
            let mut items = items?;
            let wait = clock.sleep(ANSWER_TIMEOUT);
            let next = match select(items.next(), wait).await {
                Either::Left((next, _)) => next,
                Either::Right(((), _)) => {
                    return Some((
                        Err(StorageError::Unavailable(Internal::new(format!(
                            "the bucket sent nothing more of a {} within {} seconds",
                            call.operation(),
                            ANSWER_TIMEOUT.as_secs()
                        )))),
                        None,
                    ));
                }
            };
            match next? {
                Ok(item) => Some((Ok(item), Some(items))),
                Err(error) => Some((Err(error), None)),
            }
        }
    })
    .boxed()
}

/// The HTTP client of the bucket's requests, with TLS through rustls with its ring provider
/// (SEC-16), which verifies an `https` bucket against the operating system's certificates, or the
/// files that `SSL_CERT_FILE` and `SSL_CERT_DIR` name, as a verified database connection does
/// (D38), and against those of `ca_path`, if it names a file. Its only timeout is the
/// connection's: the waits for the bucket's answers are the clock's, in [`Storage`]. It follows
/// no redirect, and goes through no proxy, whatever the environment's variables, such as
/// `HTTPS_PROXY`, name, so a request reaches the endpoint that the settings name, or fails
/// (SEC-10).
pub(crate) fn client(ca_path: Option<&std::path::Path>) -> Result<reqwest::Client, Internal> {
    let tls = crate::tls::client_config_trusting(ca_path).map_err(Internal::new)?;
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .tls_backend_preconfigured(tls)
        .build()
        .map_err(|error| Internal::new(format!("the bucket's client: {}", error.without_url())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_094_a_write_s_url_signs_its_media_type_and_lasts_a_minute() {
        let endpoint = "https://s3.example.com".parse().unwrap();
        let bucket = Bucket::new(endpoint, UrlStyle::VirtualHost, "files", "eu-1").unwrap();
        let credentials = Credentials::new("an-id", "a-secret");

        let url = writing(&bucket, &credentials, "avatars/0a", "image/png");

        assert_eq!(url.host_str(), Some("files.s3.example.com"));
        assert_eq!(url.path(), "/avatars/0a");
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["X-Amz-Algorithm"], "AWS4-HMAC-SHA256");
        assert_eq!(query["X-Amz-Expires"], "60");
        assert_eq!(query["X-Amz-SignedHeaders"], "content-type;host");
        assert!(query["X-Amz-Credential"].starts_with("an-id/"));
        assert_eq!(query["X-Amz-Signature"].len(), 64);
        assert!(!url.as_str().contains("a-secret"));
    }
}
