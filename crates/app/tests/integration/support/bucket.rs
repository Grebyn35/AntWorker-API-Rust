//! The tests' buckets, in the SeaweedFS that `just services` starts, which takes no credentials:
//! each test that runs the API has a bucket of its own, named after its database, so the jobs of
//! one test's files never meet another's objects, and its database's drop empties and deletes it,
//! so the tests leave nothing in the bucket (OPS-21).

use std::net::SocketAddr;

use futures_util::TryStreamExt;
use platform::{
    storage::{Storage, StorageConfig},
    types::Secret,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

/// The address of the SeaweedFS that `just services` starts.
const ADDRESS: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8333);

/// The endpoint the tests' API reaches the bucket at.
pub const ENDPOINT: &str = "http://127.0.0.1:8333";

/// The bucket of the test whose database is `database`: its name in lowercase, with dashes for
/// underscores, as a bucket's name allows.
pub fn name(database: &str) -> String {
    database.to_lowercase().replace('_', "-")
}

/// The client of the bucket `bucket`, as the API builds it.
pub fn storage(bucket: &str) -> Storage {
    Storage::new(&StorageConfig {
        endpoint: ENDPOINT.parse().unwrap(),
        region: "us-east-1".to_owned(),
        bucket: bucket.to_owned(),
        access_key_id: "test".to_owned(),
        secret_access_key: Secret::new("test".to_owned()),
        path_style: true,
        ca_path: None,
    })
    .unwrap()
}

/// Creates the bucket `bucket`.
pub async fn create(bucket: &str) {
    let status = request("PUT", bucket).await;
    assert!(status == 200, "create the bucket {bucket}: {status}");
}

/// Deletes every object of the bucket `bucket`, then the bucket, which S3 deletes only empty.
pub async fn remove(bucket: &str) {
    let storage = storage(bucket);
    let keys: Vec<_> = storage.list("").try_collect().await.unwrap();
    for key in keys {
        storage.delete(&key).await.unwrap();
    }
    let status = request("DELETE", bucket).await;
    assert!(
        status == 204 || status == 200,
        "delete the bucket {bucket}: {status}"
    );
}

/// Sends `method` for the bucket `bucket`, unsigned, as SeaweedFS takes it without credentials,
/// and returns the status. The request is HTTP/1.0, so the answer ends with the connection.
async fn request(method: &str, bucket: &str) -> u16 {
    let mut stream = TcpStream::connect(ADDRESS)
        .await
        .expect("SeaweedFS, which `just services` starts");
    let request =
        format!("{method} /{bucket} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
        .split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .unwrap_or_default()
}
