//! The client of the S3-compatible bucket that holds files' bytes (D1, D14, the files spec's
//! "Storage"). It reports a bucket that doesn't answer as unavailable, and every other failure as
//! internal, neither naming the key, so no error of the library leaves it (ERR-04).

mod failures;

use std::{ops::Range, path::PathBuf, sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::{
    StreamExt,
    future::{Either, select},
    stream::{self, BoxStream},
};
use object_store::{
    Attribute, Attributes, ClientOptions, GetOptions, GetRange, ObjectStore, ObjectStoreExt,
    PutOptions, PutPayload, RetryConfig,
    aws::{AmazonS3, AmazonS3Builder},
    client::{HttpClient, HttpConnector},
    path::Path,
};
use tracing::Instrument;
use url::Url;

use self::failures::classify;
use crate::{
    clock::SharedClock,
    errors::Internal,
    random,
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

/// How long a failed request is retried, from its first attempt: long enough for the failures
/// that come at once, such as a refused connection or a busy bucket's 503: the waits above bound
/// each call, its retries included.
const RETRY_TIMEOUT: Duration = Duration::from_secs(2);

/// How many times a request is retried.
const RETRIES: usize = 2;

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
    store: Arc<AmazonS3>,
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
    /// can't serve now, with a 5xx or a 429, after the client's retries (FILES-012).
    #[error("the bucket didn't answer")]
    Unavailable(#[source] Internal),
    /// The bucket refused the request, or answered what the client couldn't read.
    #[error("the bucket's request failed")]
    Failed(#[source] Internal),
}

/// The key of an object: random, so it names no row and reveals nothing, under a prefix of its
/// module's (the files spec's "Storage").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// A new key under `prefix`: 128 bits from the operating system's secure random source, in
    /// hexadecimal (CODE-06).
    pub fn random(prefix: &str) -> Result<Self, Internal> {
        let bits = random::bytes::<16>()?;
        let hex: String = bits.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self(format!("{prefix}{hex}")))
    }

    /// A key that the database holds.
    pub fn from_stored(key: &str) -> Self {
        Self(key.to_owned())
    }

    /// The key.
    pub fn as_str(&self) -> &str {
        &self.0
    }
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
    Delete,
    List,
}

impl Call {
    /// The S3 operation, which names the call's span.
    fn operation(self) -> &'static str {
        match self {
            Self::Put => "S3 PutObject",
            Self::Get => "S3 GetObject",
            Self::Delete => "S3 DeleteObject",
            Self::List => "S3 ListObjectsV2",
        }
    }

    /// How long the bucket has to answer it.
    fn timeout(self) -> Duration {
        match self {
            Self::Put => WRITE_TIMEOUT,
            Self::Get | Self::Delete | Self::List => ANSWER_TIMEOUT,
        }
    }
}

impl Storage {
    /// The client of the bucket that `config` describes, whose waits for the bucket's answers are
    /// measured by `clock`. It reaches the bucket only when asked, so a bucket that doesn't
    /// answer at startup keeps no route from serving. Building it reads the certificates it
    /// trusts from their files, once (the platform spec's Storage).
    pub fn new(config: &StorageConfig, clock: SharedClock) -> Result<Self, Internal> {
        let options = ClientOptions::new().with_allow_http(config.endpoint.scheme() == "http");
        let retry = RetryConfig {
            max_retries: RETRIES,
            retry_timeout: RETRY_TIMEOUT,
            ..RetryConfig::default()
        };
        let store = AmazonS3Builder::new()
            .with_endpoint(config.endpoint.as_str().trim_end_matches('/'))
            .with_region(&config.region)
            .with_bucket_name(&config.bucket)
            .with_access_key_id(&config.access_key_id)
            .with_secret_access_key(config.secret_access_key.expose())
            .with_virtual_hosted_style_request(!config.path_style)
            .with_disable_tagging(true)
            .with_disable_bulk_delete(true)
            .with_client_options(options)
            .with_retry(retry)
            .with_http_connector(Connector {
                ca_path: config.ca_path.clone(),
            })
            .build()
            .map_err(Internal::new)?;
        let host = config
            .endpoint
            .host_str()
            .ok_or_else(|| Internal::new("the bucket's endpoint has no host"))?;
        Ok(Self {
            store: Arc::new(store),
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
        let mut attributes = Attributes::new();
        attributes.insert(Attribute::ContentType, media_type.to_owned().into());
        let options = PutOptions {
            attributes,
            ..PutOptions::default()
        };
        let path = path(key);
        let put = self
            .store
            .put_opts(&path, PutPayload::from_bytes(bytes), options);
        self.answer(Call::Put, put).await.map(drop)
    }

    /// The object `key`'s bytes, or those of `range` alone, offsets of its bytes that it holds,
    /// as the bucket streams them. The bucket has as long for each next chunk as for its first
    /// answer, counted from when the chunk is asked for, so a client that reads slowly never cuts
    /// its download short.
    pub async fn get(
        &self,
        key: &ObjectKey,
        range: Option<Range<u64>>,
    ) -> Result<Object, StorageError> {
        let path = path(key);
        let options = GetOptions {
            range: range.map(GetRange::Bounded),
            ..GetOptions::default()
        };
        let found = self
            .answer(Call::Get, self.store.get_opts(&path, options))
            .await?;
        let size = found.meta.size;
        // A stream that the bucket stops sending, or that fails, counts again, by its failure,
        // so a bucket that answers and then stalls still shows (OBS-14).
        let bytes = answered(found.into_stream(), self.clock.clone(), Call::Get)
            .map(|chunk| {
                chunk.map_err(|failure| {
                    count(Some(&failure));
                    failure.into_cause()
                })
            })
            .boxed();
        Ok(Object { size, bytes })
    }

    /// Deletes the object `key`. An object that isn't there counts as deleted, so a second
    /// deletion of it succeeds.
    pub async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        let path = path(key);
        match self.answer(Call::Delete, self.store.delete(&path)).await {
            Ok(()) | Err(StorageError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The objects under `prefix`, as the bucket lists them, a page at a time, in the listing's
    /// client span (OBS-06). The bucket has as long for each page as for any other answer, and
    /// the listing counts once, as it ends or fails.
    pub fn list(&self, prefix: &str) -> BoxStream<'static, Result<Listed, StorageError>> {
        let prefix = Path::from(prefix.trim_end_matches('/'));
        let listed = self.store.list(Some(&prefix)).map(|listed| {
            listed.map(|meta| Listed {
                key: ObjectKey(meta.location.to_string()),
                // A time S3 gives is a whole second's, which a timestamp in seconds always holds.
                written_at: time::OffsetDateTime::from_unix_timestamp(
                    meta.last_modified.timestamp(),
                )
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH),
            })
        });
        let keys = answered(listed, self.clock.clone(), Call::List);
        stream::unfold(Some((keys, self.span(Call::List))), |state| async move {
            let (mut keys, span) = state?;
            match keys.next().instrument(span.clone()).await {
                None => {
                    count(None);
                    None
                }
                Some(Ok(key)) => Some((Ok(key), Some((keys, span)))),
                Some(Err(error)) => {
                    count(Some(&error));
                    Some((Err(error), None))
                }
            }
        })
        .boxed()
    }

    /// Waits for the bucket's answer to `call`, which `answering` awaits, in the call's client
    /// span (OBS-06), for at most the call's timeout on the clock, and counts how it ended.
    async fn answer<T>(
        &self,
        call: Call,
        answering: impl Future<Output = object_store::Result<T>> + Send,
    ) -> Result<T, StorageError> {
        let wait = self.clock.sleep(call.timeout());
        let answered = async {
            match select(std::pin::pin!(answering), wait).await {
                Either::Left((answer, _)) => answer.map_err(|error| classify(&error, call)),
                Either::Right(((), _)) => Err(StorageError::Unavailable(Internal::new(format!(
                    "the bucket didn't answer a {} within {} seconds",
                    call.operation(),
                    call.timeout().as_secs()
                )))),
            }
        }
        .instrument(self.span(call))
        .await;
        count(answered.as_ref().map(drop).err());
        answered
    }

    /// The client span of `call` (OBS-06), which names the bucket's address and never the key.
    fn span(&self, call: Call) -> tracing::Span {
        tracing::info_span!(
            "s3",
            { names::OTEL_NAME } = call.operation(),
            { names::OTEL_KIND } = "client",
            { names::SERVER_ADDRESS } = &*self.host,
            { names::SERVER_PORT } = self.port,
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
    items: impl futures_util::Stream<Item = object_store::Result<T>> + Send + 'static,
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
                            "the bucket sent nothing more within {} seconds",
                            ANSWER_TIMEOUT.as_secs()
                        )))),
                        None,
                    ));
                }
            };
            match next? {
                Ok(item) => Some((Ok(item), Some(items))),
                Err(error) => Some((Err(classify(&error, call)), None)),
            }
        }
    })
    .boxed()
}

/// The bucket's path of `key`.
fn path(key: &ObjectKey) -> Path {
    Path::from(key.0.as_str())
}

/// The library's HTTP client, with TLS through rustls with its ring provider (SEC-16), which
/// verifies an `https` bucket against the operating system's certificates, or the files that
/// `SSL_CERT_FILE` and `SSL_CERT_DIR` name, as a verified database connection does (D38), and
/// against those of `S3_CA_PATH`, if it names a file. Its only timeout is the connection's: the
/// waits for the bucket's answers are the clock's, in [`Storage`].
#[derive(Debug)]
struct Connector {
    ca_path: Option<PathBuf>,
}

impl HttpConnector for Connector {
    fn connect(&self, _: &ClientOptions) -> object_store::Result<HttpClient> {
        let failed =
            |source: Box<dyn std::error::Error + Send + Sync>| object_store::Error::Generic {
                store: "S3",
                source,
            };
        let tls = crate::tls::client_config(true, self.ca_path.as_deref()).map_err(failed)?;
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .tls_backend_preconfigured(tls)
            .build()
            .map_err(|error| failed(Box::new(error)))?;
        Ok(HttpClient::new(client))
    }
}
