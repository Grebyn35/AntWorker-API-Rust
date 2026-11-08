//! The tests' buckets, in the SeaweedFS that `just services` starts, which takes no credentials:
//! each test that runs the API has a bucket of its own, named after its database, so the jobs of
//! one test's files never meet another's objects, and its database's drop empties and deletes it,
//! so the tests leave nothing in the bucket (OPS-21).

use std::{net::SocketAddr, sync::Arc};

use futures_util::TryStreamExt;
use platform::{
    clock::TestClock,
    storage::{Storage, StorageConfig},
    types::Secret,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
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

/// The client of the bucket `bucket`, as the API builds it, on a clock that never moves, so it
/// waits for each answer as long as the bucket takes.
pub fn storage(bucket: &str) -> Storage {
    Storage::new(
        &StorageConfig {
            endpoint: ENDPOINT.parse().unwrap(),
            region: "us-east-1".to_owned(),
            bucket: bucket.to_owned(),
            access_key_id: "test".to_owned(),
            secret_access_key: Secret::new("test".to_owned()),
            path_style: true,
            ca_path: None,
        },
        Arc::new(TestClock::default()),
    )
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
    let listed: Vec<_> = storage.list("").try_collect().await.unwrap();
    for object in listed {
        storage.delete(&object.key).await.unwrap();
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

/// A stand-in for a bucket that answers as the tests need, in place of SeaweedFS (TEST-04).
pub struct Scripted {
    /// Its endpoint, for `S3_ENDPOINT`.
    pub endpoint: String,
    server: JoinHandle<()>,
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A bucket that reads each request whole and answers it with `status` and no body, or, with
/// `None`, never answers, holding each connection open until the bucket is dropped.
pub async fn scripted(status: Option<u16>) -> Scripted {
    serve(status.map(|status| (status, String::new()))).await
}

/// A bucket that answers every request with a listing of `objects`, each a key and when the
/// bucket wrote it, as S3 writes a time: in UTC, to the millisecond, as RFC 3339 allows.
pub async fn listing(objects: &[(&str, &str)]) -> Scripted {
    let contents: String = objects
        .iter()
        .map(|(key, written)| {
            format!(
                "<Contents><Key>{key}</Key><LastModified>{written}</LastModified>\
                 <ETag>\"e\"</ETag><Size>5</Size></Contents>"
            )
        })
        .collect();
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult>\
         <IsTruncated>false</IsTruncated>{contents}</ListBucketResult>"
    );
    serve(Some((200, body))).await
}

/// Serves each connection with `answer`, a status and a body, or no answer.
async fn serve(answer: Option<(u16, String)>) -> Scripted {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(answering(stream, answer.clone()));
        }
    });
    Scripted { endpoint, server }
}

/// Reads each request on `stream`, its body included, and answers it with `answer`, or never.
async fn answering(stream: TcpStream, answer: Option<(u16, String)>) {
    let mut stream = BufReader::new(stream);
    loop {
        let mut length = 0;
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end().to_ascii_lowercase();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("content-length:") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; length];
        if stream.read_exact(&mut body).await.is_err() {
            return;
        }
        let Some((status, body)) = &answer else {
            // Holds the connection, answering nothing.
            std::future::pending::<()>().await;
            return;
        };
        let answer = format!(
            "HTTP/1.1 {status} Scripted\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        if stream.get_mut().write_all(answer.as_bytes()).await.is_err() {
            return;
        }
    }
}
