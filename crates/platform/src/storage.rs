//! The client of the S3-compatible bucket that holds files' bytes (D1, D14, the files spec's
//! "Storage"), and of its copy off the server, which the bundled bucket has (D101, the platform
//! spec's "Storage"). It reports a bucket that didn't answer as unavailable, and every other
//! failure as internal, neither naming the key, so no error of the library leaves it (ERR-04).

use std::{cmp::Ordering, path::PathBuf, sync::Arc, time::Duration};

pub use bytes::Bytes;
use futures_util::{
    StreamExt, TryStreamExt,
    stream::{self, BoxStream, Fuse},
};
use object_store::{
    Attribute, Attributes, ClientOptions, ObjectStore, ObjectStoreExt, PutOptions, PutPayload,
    RetryConfig,
    aws::{AmazonS3, AmazonS3Builder},
    client::{HttpClient, HttpConnector},
    path::Path,
};
use url::Url;

use crate::{errors::Internal, random, types::Secret};

/// How long one request to the bucket may take, its retries aside.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// How long connecting to the bucket may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a request is retried, from its first attempt, before the bucket counts as
/// unavailable: short enough that an upload or a download answers within its route's timeout.
const RETRY_TIMEOUT: Duration = Duration::from_secs(5);

/// How many times a request is retried.
const RETRIES: usize = 2;

/// The prefix under which `put` writes each object a second time, for the deployment to move to
/// the copy (PLATFORM-078).
const QUEUE: &str = "copy-queue/";

/// The object that marks the bucket as restoring from its copy, outside every module's prefix
/// (PLATFORM-078).
const RESTORING: &str = "copy-restoring";

/// What reaching the bucket of files' bytes takes, and its copy's (PLATFORM-076, PLATFORM-077).
#[derive(Clone, Debug)]
pub struct StorageConfig {
    /// The bucket.
    pub bucket: BucketConfig,
    /// Its copy off the server, which the bundled bucket has.
    pub copy: Option<BucketConfig>,
}

/// What reaching one bucket takes.
#[derive(Clone, Debug)]
pub struct BucketConfig {
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

/// The bucket, and its copy when there is one.
#[derive(Clone, Debug)]
pub struct Storage {
    bucket: Arc<AmazonS3>,
    copy: Option<Arc<AmazonS3>>,
}

/// Which of the buckets holds an object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// The bucket alone.
    Bucket,
    /// The copy alone.
    Copy,
    /// Both.
    Both,
}

/// A failure to reach the bucket.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The bucket holds no object of the key.
    #[error("the bucket holds no object of the key")]
    NotFound,
    /// The bucket didn't answer, after the client's retries (FILES-012).
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

/// An object's bytes, a chunk at a time, as the bucket streams them. A failure while they stream
/// ends the stream.
pub type ObjectBytes = BoxStream<'static, Result<Bytes, Internal>>;

/// An object's bytes, as the bucket streams them, with their size.
pub struct Object {
    /// How many bytes it holds.
    pub size: u64,
    /// Its bytes.
    pub bytes: ObjectBytes,
}

impl Storage {
    /// The clients of the bucket and of the copy that `config` describes. They reach a bucket
    /// only when asked, so a bucket that doesn't answer at startup keeps no route from serving.
    pub fn new(config: &StorageConfig) -> Result<Self, Internal> {
        Ok(Self {
            bucket: Arc::new(client(&config.bucket)?),
            copy: config.copy.as_ref().map(client).transpose()?.map(Arc::new),
        })
    }

    /// Writes `bytes` as the object `key`, of media type `media_type`, and, with a copy, again
    /// under the copy's queue, both at once, failing unless both writes succeed (PLATFORM-078).
    pub async fn put(
        &self,
        key: &ObjectKey,
        bytes: Bytes,
        media_type: &str,
    ) -> Result<(), StorageError> {
        let write = |path: Path, bytes: Bytes| {
            let mut attributes = Attributes::new();
            attributes.insert(Attribute::ContentType, media_type.to_owned().into());
            let options = PutOptions {
                attributes,
                ..PutOptions::default()
            };
            let bucket = Arc::clone(&self.bucket);
            async move {
                bucket
                    .put_opts(&path, PutPayload::from_bytes(bytes), options)
                    .await
                    .map(drop)
                    .map_err(classify)
            }
        };
        if self.copy.is_none() {
            return write(path(key), bytes).await;
        }
        let queued = Path::from(format!("{QUEUE}{}", key.0));
        futures_util::try_join!(write(path(key), bytes.clone()), write(queued, bytes)).map(drop)
    }

    /// The object `key`'s bytes, as the bucket streams them.
    pub async fn get(&self, key: &ObjectKey) -> Result<Object, StorageError> {
        let found = self.bucket.get(&path(key)).await.map_err(classify)?;
        let size = found.meta.size;
        let bytes = found.into_stream().map_err(Internal::new).boxed();
        Ok(Object { size, bytes })
    }

    /// Deletes the object `key` from the copy, if there is one, then from the bucket, so that a
    /// deletion that stops between the two leaves the object in the bucket, where the next
    /// deletion finds it (PLATFORM-078). An object that isn't there counts as deleted, so a
    /// second deletion of it succeeds.
    pub async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        if let Some(copy) = &self.copy {
            deleted(copy, key).await?;
        }
        deleted(&self.bucket, key).await
    }

    /// The keys of the objects under `prefix` that the bucket or its copy holds, each once, in
    /// the order of the keys, a page of each bucket's list at a time (PLATFORM-078).
    pub fn list(&self, prefix: &str) -> BoxStream<'static, Result<ObjectKey, StorageError>> {
        self.holdings(prefix).map_ok(|(key, _)| key).boxed()
    }

    /// The keys of the objects under `prefix` that the bucket or its copy holds, each once, in
    /// the order of the keys, with which of the two holds it. Each bucket lists its keys in
    /// order, as S3's `ListObjectsV2` does, and a list that doesn't fails the stream, rather than
    /// merging the two wrongly.
    pub fn holdings(
        &self,
        prefix: &str,
    ) -> BoxStream<'static, Result<(ObjectKey, Held), StorageError>> {
        let merge = Merge {
            bucket: keys(&self.bucket, Some(prefix)).fuse(),
            copy: self
                .copy
                .as_ref()
                .map(|copy| keys(copy, Some(prefix)).fuse()),
            next_bucket: None,
            next_copy: None,
            last: None,
        };
        stream::try_unfold(merge, |mut merge| async move {
            Ok(merge.next().await?.map(|holding| (holding, merge)))
        })
        .boxed()
    }

    /// Whether the bucket holds no object at all, as a new server's doesn't, from the first
    /// page of its list.
    pub async fn is_empty(&self) -> Result<bool, StorageError> {
        Ok(keys(&self.bucket, None).next().await.transpose()?.is_none())
    }

    /// Whether the bucket is restoring from its copy: whether it holds the object that marks it
    /// so, which only a bucket with a copy can (PLATFORM-078).
    pub async fn restoring(&self) -> Result<bool, StorageError> {
        if self.copy.is_none() {
            return Ok(false);
        }
        match self
            .bucket
            .head(&Path::from(RESTORING))
            .await
            .map_err(classify)
        {
            Ok(_) => Ok(true),
            Err(StorageError::NotFound) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Marks the bucket as restoring from its copy, for the deployment to bring back every
    /// object of the copy, which then removes the mark (the files spec's "Restoring from the
    /// copy").
    pub async fn mark_restoring(&self) -> Result<(), StorageError> {
        self.bucket
            .put(&Path::from(RESTORING), PutPayload::default())
            .await
            .map(drop)
            .map_err(classify)
    }
}

/// The client of the bucket that `config` describes.
fn client(config: &BucketConfig) -> Result<AmazonS3, Internal> {
    let options = ClientOptions::new()
        .with_timeout(REQUEST_TIMEOUT)
        .with_connect_timeout(CONNECT_TIMEOUT)
        .with_allow_http(config.endpoint.scheme() == "http");
    let retry = RetryConfig {
        max_retries: RETRIES,
        retry_timeout: RETRY_TIMEOUT,
        ..RetryConfig::default()
    };
    AmazonS3Builder::new()
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
        .map_err(Internal::new)
}

/// Deletes the object `key` from `bucket`, where one that isn't there counts as deleted.
async fn deleted(bucket: &AmazonS3, key: &ObjectKey) -> Result<(), StorageError> {
    match bucket.delete(&path(key)).await.map_err(classify) {
        Ok(()) | Err(StorageError::NotFound) => Ok(()),
        Err(error) => Err(error),
    }
}

/// The keys of the objects of `bucket` under `prefix`, or every object's, as the bucket lists
/// them, a page at a time.
fn keys(
    bucket: &Arc<AmazonS3>,
    prefix: Option<&str>,
) -> BoxStream<'static, Result<ObjectKey, StorageError>> {
    let prefix = prefix.map(|prefix| Path::from(prefix.trim_end_matches('/')));
    Arc::clone(bucket)
        .list(prefix.as_ref())
        .map(|listed| {
            listed
                .map(|meta| ObjectKey(meta.location.to_string()))
                .map_err(classify)
        })
        .boxed()
}

/// The merge of the bucket's list with its copy's, each in the order of the keys, and fused, so
/// that one which has ended is asked again for nothing.
struct Merge {
    bucket: Fuse<BoxStream<'static, Result<ObjectKey, StorageError>>>,
    copy: Option<Fuse<BoxStream<'static, Result<ObjectKey, StorageError>>>>,
    next_bucket: Option<ObjectKey>,
    next_copy: Option<ObjectKey>,
    last: Option<ObjectKey>,
}

impl Merge {
    /// The next key that either list holds, with which holds it, or `None` once both have
    /// ended.
    async fn next(&mut self) -> Result<Option<(ObjectKey, Held)>, StorageError> {
        if self.next_bucket.is_none() {
            self.next_bucket = self.bucket.next().await.transpose()?;
        }
        if self.next_copy.is_none()
            && let Some(copy) = &mut self.copy
        {
            self.next_copy = copy.next().await.transpose()?;
        }
        let held = match (&self.next_bucket, &self.next_copy) {
            (None, None) => return Ok(None),
            (Some(_), None) => Held::Bucket,
            (None, Some(_)) => Held::Copy,
            (Some(bucket), Some(copy)) => match bucket.0.cmp(&copy.0) {
                Ordering::Less => Held::Bucket,
                Ordering::Greater => Held::Copy,
                Ordering::Equal => Held::Both,
            },
        };
        let key = match held {
            Held::Bucket => self.next_bucket.take(),
            Held::Copy => self.next_copy.take(),
            Held::Both => self.next_copy.take().and(self.next_bucket.take()),
        }
        .ok_or_else(|| StorageError::Failed(Internal::new("a list's next key went missing")))?;
        if self.last.as_ref().is_some_and(|last| key.0 <= last.0) {
            return Err(StorageError::Failed(Internal::new(
                "a bucket listed its keys out of order",
            )));
        }
        self.last = Some(key.clone());
        Ok(Some((key, held)))
    }
}

/// The bucket's path of `key`.
fn path(key: &ObjectKey) -> Path {
    Path::from(key.0.as_str())
}

/// The failure `error` stands for: an object that isn't there, a request the bucket refused, or
/// one it didn't answer, after the client's retries, which the library reports as a generic
/// failure.
fn classify(error: object_store::Error) -> StorageError {
    match error {
        object_store::Error::NotFound { .. } => StorageError::NotFound,
        object_store::Error::Generic { .. } => StorageError::Unavailable(Internal::new(error)),
        _ => StorageError::Failed(Internal::new(error)),
    }
}

/// The library's HTTP client, with TLS through rustls with its ring provider (SEC-16), which
/// verifies an `https` bucket against the operating system's certificates, or the files that
/// `SSL_CERT_FILE` and `SSL_CERT_DIR` name, as a verified database connection does (D38), and
/// against those of `S3_CA_PATH`, if it names a file.
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
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .tls_backend_preconfigured(tls)
            .build()
            .map_err(|error| failed(Box::new(error)))?;
        Ok(HttpClient::new(client))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bucket's settings, with `ca_path`.
    fn config(ca_path: Option<PathBuf>) -> StorageConfig {
        StorageConfig {
            bucket: BucketConfig {
                endpoint: "https://s3.example.com".parse().unwrap(),
                region: "us-east-1".to_owned(),
                bucket: "files".to_owned(),
                access_key_id: "key".to_owned(),
                secret_access_key: Secret::new("secret".to_owned()),
                path_style: false,
                ca_path,
            },
            copy: None,
        }
    }

    #[test]
    fn platform_076_the_client_trusts_the_authorities_of_s3_ca_path() {
        assert!(Storage::new(&config(None)).is_ok());
        let missing = PathBuf::from("/nonexistent/files-ca.crt");
        assert!(Storage::new(&config(Some(missing))).is_err());
    }
}
