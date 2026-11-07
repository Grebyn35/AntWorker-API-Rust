//! The client of the S3-compatible bucket that holds files' bytes (D1, D14, the files spec's
//! "Storage"). It reports a bucket that didn't answer as unavailable, and every other failure as
//! internal, neither naming the key, so no error of the library leaves it (ERR-04).

use std::{path::PathBuf, sync::Arc, time::Duration};

pub use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt, stream::BoxStream};
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
#[derive(Clone, Debug)]
pub struct Storage {
    store: Arc<AmazonS3>,
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
    /// The client of the bucket that `config` describes. It reaches the bucket only when asked,
    /// so a bucket that doesn't answer at startup keeps no route from serving.
    pub fn new(config: &StorageConfig) -> Result<Self, Internal> {
        let options = ClientOptions::new()
            .with_timeout(REQUEST_TIMEOUT)
            .with_connect_timeout(CONNECT_TIMEOUT)
            .with_allow_http(config.endpoint.scheme() == "http");
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
        Ok(Self {
            store: Arc::new(store),
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
        self.store
            .put_opts(&path(key), PutPayload::from_bytes(bytes), options)
            .await
            .map(drop)
            .map_err(classify)
    }

    /// The object `key`'s bytes, as the bucket streams them.
    pub async fn get(&self, key: &ObjectKey) -> Result<Object, StorageError> {
        let found = self.store.get(&path(key)).await.map_err(classify)?;
        let size = found.meta.size;
        let bytes = found.into_stream().map_err(Internal::new).boxed();
        Ok(Object { size, bytes })
    }

    /// Deletes the object `key`. An object that isn't there counts as deleted, so a second
    /// deletion of it succeeds.
    pub async fn delete(&self, key: &ObjectKey) -> Result<(), StorageError> {
        match self.store.delete(&path(key)).await.map_err(classify) {
            Ok(()) | Err(StorageError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The keys of the objects under `prefix`, as the bucket lists them, a page at a time.
    pub fn list(&self, prefix: &str) -> BoxStream<'static, Result<ObjectKey, StorageError>> {
        let prefix = Path::from(prefix.trim_end_matches('/'));
        self.store
            .list(Some(&prefix))
            .map(|listed| {
                listed
                    .map(|meta| ObjectKey(meta.location.to_string()))
                    .map_err(classify)
            })
            .boxed()
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
            endpoint: "https://s3.example.com".parse().unwrap(),
            region: "us-east-1".to_owned(),
            bucket: "files".to_owned(),
            access_key_id: "key".to_owned(),
            secret_access_key: Secret::new("secret".to_owned()),
            path_style: false,
            ca_path,
        }
    }

    #[test]
    fn platform_076_the_client_trusts_the_authorities_of_s3_ca_path() {
        assert!(Storage::new(&config(None)).is_ok());
        let missing = PathBuf::from("/nonexistent/files-ca.crt");
        assert!(Storage::new(&config(Some(missing))).is_err());
    }
}
