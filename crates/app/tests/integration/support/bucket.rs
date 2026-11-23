//! The tests' buckets, in the SeaweedFS that `just services` starts, which takes no credentials:
//! each test that runs the API has a bucket of its own, named after its database, so the jobs of
//! one test's files never meet another's objects. The process's cleanup empties and deletes it
//! once the test has ended, and a later run deletes, by their names, the buckets that a process
//! which runs no more left, so the tests leave nothing in the storage (OPS-21).

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, PoisonError},
};

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

/// The address of its filer, which lists every bucket as an entry of `/buckets/`: its S3 API
/// lists a caller only the buckets that are the caller's own, and the tests' requests, unsigned,
/// are no one's.
const FILER: SocketAddr =
    SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8334);

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
    let status = request("PUT", bucket, "").await;
    assert!(status == 200, "create the bucket {bucket}: {status}");
}

/// Has the bucket `bucket` keep a version of each object that is deleted or replaced in it, as
/// S3's versioning does (OPS-22).
pub async fn keep_versions(bucket: &str) {
    let on = "<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
              <Status>Enabled</Status></VersioningConfiguration>";
    let status = request("PUT", &format!("{bucket}?versioning"), on).await;
    assert!(status == 200, "keep the versions of {bucket}: {status}");
}

/// Deletes the version `id` of the object `key` of the bucket `bucket`, as only a key pair that
/// the server doesn't hold may in a deployment (OPS-22).
pub async fn delete_version(bucket: &str, key: &str, id: &str) {
    let mut url = url::Url::parse(ENDPOINT).unwrap();
    url.path_segments_mut()
        .unwrap()
        .push(bucket)
        .extend(key.split('/'));
    url.query_pairs_mut().append_pair("versionId", id);
    let target = format!("{}?{}", url.path(), url.query().unwrap());
    let status = request("DELETE", target.trim_start_matches('/'), "").await;
    assert!(status == 204, "delete a version of {bucket}: {status}");
}

/// The names of the storage's buckets that start with `prefix`, from its filer's listing.
pub async fn named(prefix: &str) -> Vec<String> {
    let mut stream = TcpStream::connect(FILER)
        .await
        .expect("the filer of SeaweedFS, which `just services` starts");
    // HTTP/1.0, so the answer ends with the connection, and more entries than any server holds.
    let request = "GET /buckets/?limit=1000000 HTTP/1.0\r\nHost: 127.0.0.1\r\n\
                   Accept: application/json\r\n\r\n";
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (head, body) = response
        .split_once("\r\n\r\n")
        .expect("the filer's answer has a body");
    assert!(head.contains(" 200 "), "list the buckets: {head}");
    let listing: serde_json::Value = serde_json::from_str(body).expect("the filer's listing");
    listing["Entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["FullPath"].as_str()?.strip_prefix("/buckets/"))
        .filter(|name| name.starts_with(prefix))
        .map(str::to_owned)
        .collect()
}

/// Deletes every version of every object of the bucket `bucket`, then the bucket, which S3
/// deletes only empty. A bucket that keeps no versions lists each object as one version.
pub async fn remove(bucket: &str) {
    let versions: Vec<_> = storage(bucket).versions("").try_collect().await.unwrap();
    for version in versions {
        delete_version(bucket, version.key.as_str(), &version.id).await;
    }
    let status = request("DELETE", bucket, "").await;
    assert!(
        status == 204 || status == 200,
        "delete the bucket {bucket}: {status}"
    );
}

/// Sends `method` for `target`, a bucket's name, with the path of an object and a query if it
/// has them, and `body`, unsigned, as SeaweedFS takes it without credentials, and returns the
/// status. The request is HTTP/1.0, so the answer ends with the connection.
async fn request(method: &str, target: &str, body: &str) -> u16 {
    let mut stream = TcpStream::connect(ADDRESS)
        .await
        .expect("SeaweedFS, which `just services` starts");
    let request = format!(
        "{method} /{target} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
        .split(' ')
        .nth(1)
        .and_then(|status| status.parse().ok())
        .unwrap_or_default()
}

/// Waits until the storage's clock has passed into another second than the one that its answers
/// show now, so what a test writes next the storage stamps later than what it wrote before: a
/// state that the storage reaches on its own, which the wait polls (TEST-06).
pub async fn next_second() {
    let shown = date().await;
    while date().await == shown {
        tokio::task::yield_now().await;
    }
}

/// The storage's clock, to the second, as the `Date` of an answer of its own gives it.
async fn date() -> String {
    let mut stream = TcpStream::connect(ADDRESS)
        .await
        .expect("SeaweedFS, which `just services` starts");
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("date")
                .then(|| value.trim().to_owned())
        })
        .expect("the storage's answer has a date")
}

/// What a stand-in bucket keeps of one write or one deletion of an object: a version, as
/// [`keeping`] lists it and serves it.
#[derive(Clone)]
pub struct Kept {
    key: &'static str,
    id: &'static str,
    latest: bool,
    written: &'static str,
    bytes: Option<&'static [u8]>,
    size: Option<u64>,
}

impl Kept {
    /// A version of the object `key`, with the ID `id`, which holds `bytes`, of the media type
    /// `text/plain`: whether the listing calls it the latest, and when the bucket wrote it, as
    /// S3 writes a time. Its entity tag is the text of its first 16 bytes, so two versions of
    /// the same bytes share one.
    pub fn version(
        key: &'static str,
        id: &'static str,
        latest: bool,
        written: &'static str,
        bytes: &'static [u8],
    ) -> Self {
        Self {
            key,
            id,
            latest,
            written,
            bytes: Some(bytes),
            size: None,
        }
    }

    /// A delete marker of the object `key`, as [`Kept::version`] makes a version.
    pub fn marker(
        key: &'static str,
        id: &'static str,
        latest: bool,
        written: &'static str,
    ) -> Self {
        Self {
            key,
            id,
            latest,
            written,
            bytes: None,
            size: None,
        }
    }

    /// The version, which the listing gives the size `size`, whatever its bytes are.
    pub fn listed_at(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    /// The element of S3's listing of versions that lists it.
    fn listed(&self) -> String {
        let shared = format!(
            "<Key>{}</Key><VersionId>{}</VersionId><IsLatest>{}</IsLatest>\
             <LastModified>{}</LastModified>",
            self.key, self.id, self.latest, self.written
        );
        match self.bytes {
            Some(bytes) => format!(
                "<Version>{shared}<ETag>&#34;{}&#34;</ETag><Size>{}</Size></Version>",
                String::from_utf8_lossy(&bytes[..bytes.len().min(16)]),
                self.size
                    .unwrap_or_else(|| u64::try_from(bytes.len()).unwrap())
            ),
            None => format!("<DeleteMarker>{shared}</DeleteMarker>"),
        }
    }
}

/// An object that a stand-in bucket was asked to write: its key, its media type and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    pub key: String,
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// A stand-in for a bucket that keeps versions, in place of SeaweedFS, whose clock a test can't
/// set (TEST-04).
pub struct Keeping {
    /// Its endpoint, for `S3_ENDPOINT`.
    pub endpoint: String,
    written: Arc<Mutex<Vec<Written>>>,
    server: JoinHandle<()>,
}

impl Keeping {
    /// Each object that the bucket was asked to write, in their order, those it refused among
    /// them.
    pub fn written(&self) -> Vec<Written> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for Keeping {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A bucket that keeps `versions`: it answers a listing of versions with those whose keys start
/// with the listing's prefix, in their order, `page` at a page, each page after the first
/// starting after the version that the request's markers name; a read of a version with its
/// bytes, or 404; and a write with 200, or, from the write of the number `refused` on, counted
/// from 0, with 500. It answers any other request with 400.
pub async fn keeping(versions: Vec<Kept>, page: usize, refused: Option<usize>) -> Keeping {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let written = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::new((versions, page, refused, written.clone()));
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(keeping_answers(stream, kept.clone()));
        }
    });
    Keeping {
        endpoint,
        written,
        server,
    }
}

/// What [`keeping`] keeps: the versions, how many a page lists, the first write it refuses, and
/// the writes it was asked for.
type KeptState = (Vec<Kept>, usize, Option<usize>, Arc<Mutex<Vec<Written>>>);

/// Reads each request on `stream`, and answers it as [`keeping`] says.
async fn keeping_answers(stream: TcpStream, kept: Arc<KeptState>) {
    let mut stream = BufReader::new(stream);
    loop {
        let mut request = String::new();
        if stream.read_line(&mut request).await.unwrap_or(0) == 0 {
            return;
        }
        let (mut length, mut media_type) = (0, String::new());
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let Some((name, value)) = line.trim_end().split_once(':') else {
                break;
            };
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(0),
                "content-type" => value.trim().clone_into(&mut media_type),
                _ => {}
            }
        }
        let mut body = vec![0; length];
        if stream.read_exact(&mut body).await.is_err() {
            return;
        }
        let mut words = request.split(' ');
        let (method, target) = (
            words.next().unwrap_or_default(),
            words.next().unwrap_or_default(),
        );
        let (status, media, answer) = kept_answer(&kept, method, target, media_type, body);
        let head = format!(
            "HTTP/1.1 {status} Kept\r\nContent-Type: {media}\r\nContent-Length: {}\r\n\r\n",
            answer.len()
        );
        let stream = stream.get_mut();
        if stream.write_all(head.as_bytes()).await.is_err()
            || stream.write_all(&answer).await.is_err()
        {
            return;
        }
    }
}

/// The status, the media type and the body of the answer of [`keeping`] to the request of
/// `method` for `target`, whose body is `body`, of the media type `media_type`.
fn kept_answer(
    kept: &KeptState,
    method: &str,
    target: &str,
    media_type: String,
    body: Vec<u8>,
) -> (u16, &'static str, Vec<u8>) {
    let (versions, page, refused, written) = kept;
    let url = url::Url::parse(&format!("http://bucket{target}")).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    // The path holds the bucket's name, then the key.
    let path = percent_encoding::percent_decode_str(url.path())
        .decode_utf8_lossy()
        .into_owned();
    let key = path
        .trim_start_matches('/')
        .split_once('/')
        .map_or("", |(_, key)| key)
        .to_owned();
    match method {
        "GET" if query.contains_key("versions") => {
            let prefix = query.get("prefix").map_or("", String::as_str);
            let listed: Vec<&Kept> = versions
                .iter()
                .filter(|version| version.key.starts_with(prefix))
                .collect();
            let start = match (query.get("key-marker"), query.get("version-id-marker")) {
                (Some(key), Some(id)) => listed
                    .iter()
                    .position(|version| version.key == key && version.id == id)
                    .map_or(listed.len(), |last| last + 1),
                _ => 0,
            };
            let shown = &listed[start..listed.len().min(start + page)];
            let entries: String = shown.iter().map(|version| version.listed()).collect();
            let more = match shown.last() {
                Some(last) if start + shown.len() < listed.len() => format!(
                    "<IsTruncated>true</IsTruncated><NextKeyMarker>{}</NextKeyMarker>\
                     <NextVersionIdMarker>{}</NextVersionIdMarker>",
                    last.key, last.id
                ),
                _ => "<IsTruncated>false</IsTruncated>".to_owned(),
            };
            let listing = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                 <ListVersionsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 {more}{entries}</ListVersionsResult>"
            );
            (200, "application/xml", listing.into_bytes())
        }
        "GET" => {
            let found = versions.iter().find(|version| {
                version.key == key && query.get("versionId").is_some_and(|id| id == version.id)
            });
            match found.and_then(|version| version.bytes) {
                Some(bytes) => (200, "text/plain", bytes.to_vec()),
                None => (404, "application/xml", Vec::new()),
            }
        }
        "PUT" => {
            let mut written = written.lock().unwrap_or_else(PoisonError::into_inner);
            let status = if refused.is_some_and(|first| written.len() >= first) {
                500
            } else {
                200
            };
            written.push(Written {
                key,
                media_type,
                bytes: body,
            });
            (status, "application/xml", Vec::new())
        }
        _ => (400, "application/xml", Vec::new()),
    }
}

/// A stand-in for a bucket that answers as the tests need, in place of SeaweedFS (TEST-04).
pub struct Scripted {
    /// Its endpoint, for `S3_ENDPOINT`.
    pub endpoint: String,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    server: JoinHandle<()>,
}

impl Scripted {
    /// The path of each request that the bucket has read, in their order.
    pub fn paths(&self) -> Vec<String> {
        let requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        requests.iter().map(|(_, path)| path.clone()).collect()
    }

    /// The method of each request that the bucket has read, in their order.
    pub fn methods(&self) -> Vec<String> {
        let requests = self.requests.lock().unwrap_or_else(PoisonError::into_inner);
        requests.iter().map(|(method, _)| method.clone()).collect()
    }
}

/// What a scripted bucket answers each request with: a status, the `Location` of a redirect, and
/// a body, of which it sends only the first half when `cut`.
#[derive(Clone)]
struct Answer {
    status: u16,
    location: Option<&'static str>,
    body: String,
    cut: bool,
}

impl Answer {
    fn of(status: u16, body: String) -> Self {
        Self {
            status,
            location: None,
            body,
            cut: false,
        }
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A bucket that reads each request whole and answers it with `status` and no body, or, with
/// `None`, never answers, holding each connection open until the bucket is dropped.
pub async fn scripted(status: Option<u16>) -> Scripted {
    serve(status.map(|status| Answer::of(status, String::new()))).await
}

/// A bucket that answers every request with `object` as an object's bytes, and a `HEAD` with
/// their length and none of them.
pub async fn giving(object: &str) -> Scripted {
    serve(Some(Answer::of(200, object.to_owned()))).await
}

/// A bucket that answers every request with `object` as an object's bytes, as [`giving`] does,
/// but sends only the first half of them, and then closes the connection, as a bucket that stops
/// midway does.
pub async fn cutting(object: &str) -> Scripted {
    serve(Some(Answer {
        cut: true,
        ..Answer::of(200, object.to_owned())
    }))
    .await
}

/// A bucket that answers every request with a redirect to `location`, a path of its own.
pub async fn redirecting(location: &'static str) -> Scripted {
    serve(Some(Answer {
        status: 307,
        location: Some(location),
        body: String::new(),
        cut: false,
    }))
    .await
}

/// A bucket that answers every request with a listing of `objects`, in S3's namespace, each a
/// key and when the bucket wrote it, as S3 writes a time: in UTC, to the millisecond, as RFC 3339
/// allows.
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
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <IsTruncated>false</IsTruncated>{contents}</ListBucketResult>"
    );
    serve(Some(Answer::of(200, body))).await
}

/// Serves each connection with `answer`, or no answer.
async fn serve(answer: Option<Answer>) -> Scripted {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let read = requests.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(answering(stream, answer.clone(), read.clone()));
        }
    });
    Scripted {
        endpoint,
        requests,
        server,
    }
}

/// Reads each request on `stream`, its body included, records its method and its path in
/// `requests`, and answers it with `answer`, without the body when the request is a `HEAD`, or
/// never. An answer that is `cut` closes the connection once it is sent.
async fn answering(
    stream: TcpStream,
    answer: Option<Answer>,
    requests: Arc<Mutex<Vec<(String, String)>>>,
) {
    let mut stream = BufReader::new(stream);
    loop {
        let mut length = 0;
        let mut first = true;
        let mut head = false;
        loop {
            let mut line = String::new();
            if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            if std::mem::take(&mut first) {
                // The request's line: its method, its target and the protocol's version.
                let mut parts = line.split(' ');
                let method = parts.next().unwrap_or_default().to_owned();
                let target = parts.next().unwrap_or_default();
                let path = target.split('?').next().unwrap_or_default().to_owned();
                head = method == "HEAD";
                requests
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((method, path));
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
        let Some(Answer {
            status,
            location,
            body,
            cut,
        }) = &answer
        else {
            // Holds the connection, answering nothing.
            std::future::pending::<()>().await;
            return;
        };
        let location = location.map_or_else(String::new, |to| format!("Location: {to}\r\n"));
        let sent = match (head, *cut) {
            (true, _) => "",
            (false, true) => &body[..body.len() / 2],
            (false, false) => body,
        };
        let answer = format!(
            "HTTP/1.1 {status} Scripted\r\n{location}Content-Length: {}\r\n\r\n{sent}",
            body.len()
        );
        if stream.get_mut().write_all(answer.as_bytes()).await.is_err() || *cut {
            return;
        }
    }
}
