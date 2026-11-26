//! A workspace's files (the files spec), and in its modules a user's own, the allowance of a
//! day's uploads, the links to files' bytes, public files, ranges of a download's bytes, users'
//! avatars, the picture that a provider gives for a new account, workspaces' pictures, what an
//! export holds of files, and the operator's writing back of what the bucket kept, with the reads
//! of it that it stands on.

mod allowance;
mod avatars;
mod exports;
mod links;
mod own;
mod pictures;
mod public;
mod ranges;
mod restoring;
mod workspace_pictures;

use std::{sync::Arc, time::Duration};

use axum::body::Body;
use bytes::Bytes;
use futures_util::{TryStreamExt, stream};
use platform::{
    clock::TestClock,
    storage::{ObjectKey, Storage, StorageConfig},
    telemetry::testing::{self, collector::Collector},
    types::Secret,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::support::{
    Api, Server, TestDatabase,
    api::{Options, Response},
    bucket, counts,
    events::{self, Event},
    files, jobs, lengthened,
    lists::{field, pages},
    scanner::{self, Script},
    streams,
    users::{self, SignedIn},
    workspaces::{self, acme},
};

/// A body that fails once it is read, so an answer to a request that sends it shows that nothing
/// read it.
fn unread() -> Body {
    Body::from_stream(stream::once(async {
        Err::<Bytes, _>(std::io::Error::other("the body was read"))
    }))
}

/// Downloads what the link `url` names, without a credential.
async fn fetch(api: &Api, url: &str) -> Response {
    api.get(url).send().await
}

/// Uploads `bytes` as the file `name` to `workspace` as `user`, with `key` as its idempotency
/// key if given.
async fn upload(
    api: &Api,
    user: &SignedIn,
    workspace: &str,
    name: &str,
    bytes: &[u8],
    key: Option<&str>,
) -> Response {
    let disposition = format!("attachment; filename=\"{name}\"");
    upload_with(
        api,
        user,
        workspace,
        &[("content-disposition", &disposition)],
        bytes,
        key,
    )
    .await
}

/// Uploads `bytes` as the file `name` to `workspace` as `user`, with `query` as the request's
/// query, such as `?visibility=public`.
async fn upload_queried(
    api: &Api,
    user: &SignedIn,
    workspace: &str,
    query: &str,
    name: &str,
    bytes: &[u8],
) -> Response {
    api.request(
        http::Method::POST,
        &format!("/v1/workspaces/{workspace}/files{query}"),
    )
    .header("content-type", "application/octet-stream")
    .header(
        "content-disposition",
        &format!("attachment; filename=\"{name}\""),
    )
    .body(bytes.to_vec())
    .bearer(&user.access)
    .send()
    .await
}

/// Uploads `bytes` to `workspace` as `user`, with `headers` beside its media type, and `key` as
/// its idempotency key if given.
async fn upload_with(
    api: &Api,
    user: &SignedIn,
    workspace: &str,
    headers: &[(&str, &str)],
    bytes: &[u8],
    key: Option<&str>,
) -> Response {
    let mut call = api
        .request(
            http::Method::POST,
            &format!("/v1/workspaces/{workspace}/files"),
        )
        .header("content-type", "application/octet-stream")
        .body(bytes.to_vec())
        .bearer(&user.access);
    for (name, value) in headers {
        call = call.header(name, value);
    }
    if let Some(key) = key {
        call = call.header("idempotency-key", key);
    }
    call.send().await
}

/// Downloads `file` of `workspace` as `user`.
async fn download(api: &Api, user: &SignedIn, workspace: &str, file: &str) -> Response {
    api.get(&format!("/v1/workspaces/{workspace}/files/{file}/content"))
        .bearer(&user.access)
        .send()
        .await
}

/// Deletes `file` of `workspace` as `user`.
async fn delete(api: &Api, user: &SignedIn, workspace: &str, file: &str) -> Response {
    let path = format!("/v1/workspaces/{workspace}/files/{file}");
    api.request(http::Method::DELETE, &path)
        .bearer(&user.access)
        .send()
        .await
}

/// The names the files of `workspace` hold, in the order of their IDs.
async fn stored_names(api: &Api, workspace: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT name FROM workspace_files WHERE workspace_id = '{workspace}' ORDER BY id"
        ))
        .await
}

/// The keys of every object in the test's bucket.
async fn objects(api: &Api) -> Vec<String> {
    let mut keys: Vec<String> = bucket::storage(&api.database().bucket().await)
        .list("")
        .map_ok(|listed| listed.key.as_str().to_owned())
        .try_collect()
        .await
        .unwrap();
    keys.sort();
    keys
}

/// The key of the object of `file`.
async fn object_key(api: &Api, file: &str) -> String {
    api.database()
        .owner_reads(&format!(
            "SELECT object_key FROM workspace_files WHERE id = '{file}'"
        ))
        .await
        .remove(0)
}

/// Writes under `key`, in the test's bucket, an object of zeros one byte shorter than a file
/// that a builder arranges, so the bucket holds the object at another size than its row records.
async fn cut_short(api: &Api, key: &str) {
    bucket::storage(&api.database().bucket().await)
        .put(
            &ObjectKey::from_stored(key),
            Bytes::from(vec![0; files::BYTES.len() - 1]),
            "application/octet-stream",
        )
        .await
        .unwrap();
}

/// `file` without its links, which are signed for each caller's session (FILES-021).
pub(crate) fn without_links(file: &Value) -> Value {
    let mut file = file.clone();
    let fields = file.as_object_mut().unwrap();
    assert!(fields.remove("content_url").is_some());
    assert!(fields.remove("content_url_expires_at").is_some());
    file
}

/// Each audit entry of `action`, as its log, workspace, actor, resource, details and permission.
async fn entries(api: &Api, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
             || resource_type || ' ' || resource_id || ' ' || details::text \
             || ' ' || coalesce(permission, '-') FROM audit_entries WHERE action = '{action}'"
        ))
        .await
}

/// Each event named `name`, as its scope, workspace and permission, then its payload's
/// workspace, its file and how many fields it holds.
async fn published(api: &Api, name: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT scope || ' ' || workspace_id || ' ' || permission || ' ' \
             || (payload ->> 'workspace_id') || ' ' || (payload ->> 'file_id') || ' ' \
             || (SELECT count(*) FROM jsonb_object_keys(payload)) \
             FROM events WHERE name = '{name}'"
        ))
        .await
}

/// A ZIP archive whose entries, stored empty, are named `names`, as an Office document is one.
fn archive(names: &[&str]) -> Vec<u8> {
    let entries: Vec<(&str, &[u8])> = names.iter().map(|name| (*name, &[][..])).collect();
    archive_of(&entries)
}

/// A ZIP archive of `entries`, each a name and the bytes it stores, uncompressed.
fn archive_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut directory = Vec::new();
    for (name, stored) in entries {
        let offset = u32::try_from(bytes.len()).unwrap();
        let length = u16::try_from(name.len()).unwrap();
        let size = u32::try_from(stored.len()).unwrap();
        // The CRC-32 and the sizes, compressed and not, which are equal for a stored entry.
        let mut sums = platform::archive::crc32(stored).to_le_bytes().to_vec();
        sums.extend_from_slice(&size.to_le_bytes());
        sums.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(b"PK\x03\x04");
        bytes.extend_from_slice(&[0; 10]);
        bytes.extend_from_slice(&sums);
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(stored);
        directory.extend_from_slice(b"PK\x01\x02");
        directory.extend_from_slice(&[0; 12]);
        directory.extend_from_slice(&sums);
        directory.extend_from_slice(&length.to_le_bytes());
        directory.extend_from_slice(&[0; 12]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let offset = u32::try_from(bytes.len()).unwrap();
    let size = u32::try_from(directory.len()).unwrap();
    let count = u16::try_from(entries.len()).unwrap();
    bytes.extend_from_slice(&directory);
    bytes.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&size.to_le_bytes());
    bytes.extend_from_slice(&offset.to_le_bytes());
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

#[tokio::test]
async fn files_001_uploading_stores_the_file_records_it_and_publishes_it() {
    let api = Api::new().await;
    let (workspace, _, grace) = acme(&api, "member").await;

    let response = upload(&api, &grace, &workspace, "notes.txt", b"hello world", None).await;

    assert_eq!(response.status, 201);
    let body = response.json();
    let id = body["id"].as_str().unwrap();
    assert_eq!(
        response.header("location"),
        format!("/v1/workspaces/{workspace}/files/{id}")
    );
    assert_eq!(body["workspace_id"], workspace);
    assert_eq!(body["name"], "notes.txt");
    assert_eq!(body["media_type"], "text/plain; charset=utf-8");
    assert_eq!(body["size_bytes"], 11);
    assert_eq!(body["uploader_id"], grace.user_id);
    assert!(body["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(stored_names(&api, &workspace).await, ["notes.txt"]);
    assert_eq!(objects(&api).await, [object_key(&api, id).await]);
    assert_eq!(
        entries(&api, "file.uploaded").await,
        [format!(
            "workspace {workspace} {} file {id} {{}} files.read",
            grace.user_id
        )]
    );
    assert_eq!(
        published(&api, "file.uploaded").await,
        [format!(
            "workspace {workspace} files.read {workspace} {id} 2"
        )]
    );
}

#[tokio::test]
async fn files_001_a_retry_with_the_key_stores_one_file() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let first = upload(&api, &ada, &workspace, "a.txt", b"one", Some("\"k1\"")).await;
    let retry = upload(&api, &ada, &workspace, "a.txt", b"one", Some("\"k1\"")).await;
    let other = upload(&api, &ada, &workspace, "a.txt", b"two", Some("\"k1\"")).await;

    assert_eq!(first.status, 201);
    assert_eq!(retry.status, 201);
    // The retry's links are made for it, so their time may be a later second's (FILES-021).
    assert_eq!(without_links(&retry.json()), without_links(&first.json()));
    assert_eq!(retry.header("location"), first.header("location"));
    assert_eq!(other.status, 422);
    assert_eq!(other.json()["code"], "request.idempotency_key_reused");
    assert_eq!(
        other.json()["detail"],
        "The key was sent before with another request: send a new key for this one."
    );
    assert_eq!(stored_names(&api, &workspace).await, ["a.txt"]);
    assert_eq!(entries(&api, "file.uploaded").await.len(), 1);
}

#[tokio::test]
async fn files_002_a_name_comes_from_content_disposition_normalized() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let named = |disposition: String| {
        let (api, ada, workspace) = (&api, &ada, &workspace);
        async move {
            let headers = [("content-disposition", disposition.as_str())];
            upload_with(api, ada, workspace, &headers, b"x", None).await
        }
    };

    let extended = named(
        "attachment; filename=\"cafe.txt\"; filename*=UTF-8''caf%C3%A9%20%20menu.txt".to_owned(),
    )
    .await;
    let spaced = named("attachment; filename=\"  plan \t b.txt \"".to_owned()).await;
    let without = upload_with(&api, &ada, &workspace, &[], b"x", None).await;
    let unnamed = named("attachment".to_owned()).await;
    let control = named("attachment; filename*=UTF-8''a%07b.txt".to_owned()).await;
    let long = named(format!("attachment; filename=\"{}.txt\"", "a".repeat(252))).await;

    assert_eq!(extended.status, 201);
    assert_eq!(extended.json()["name"], "café menu.txt");
    assert_eq!(spaced.status, 201);
    assert_eq!(spaced.json()["name"], "plan b.txt");
    for refused in [&without, &unnamed, &control, &long] {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "files.invalid_name");
    }
    for unnamed in [&without, &unnamed] {
        assert_eq!(
            unnamed.json()["detail"],
            "Name the file in `Content-Disposition`, such as `attachment; filename*=UTF-8''report.pdf`."
        );
    }
    assert_eq!(
        long.json()["detail"],
        "A file's name has at most 255 characters."
    );
    assert_eq!(stored_names(&api, &workspace).await.len(), 2);
}

#[tokio::test]
async fn files_002_a_name_is_counted_once_normalized_too() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    // Before `.txt`, texts of 251 and 252 characters once normalized make names of 255 and 256,
    // each within the 255 as given, percent-encoded as RFC 8187 has it.
    let (longest, too_long) = (lengthened(251), lengthened(252));
    let named = async |given: &str| {
        let encoded =
            percent_encoding::utf8_percent_encode(given, percent_encoding::NON_ALPHANUMERIC);
        let disposition = format!("attachment; filename*=UTF-8''{encoded}.txt");
        let headers = [("content-disposition", disposition.as_str())];
        upload_with(&api, &ada, &workspace, &headers, b"x", None).await
    };

    let kept = named(&longest.given).await;
    let refused = named(&too_long.given).await;

    assert_eq!(kept.status, 201, "{}", kept.json());
    let normalized = format!("{}.txt", longest.normalized);
    assert_eq!(kept.json()["name"], normalized.as_str());
    assert_eq!(refused.status, 400, "{}", refused.json());
    assert_eq!(refused.json()["code"], "files.invalid_name");
    assert_eq!(stored_names(&api, &workspace).await, [normalized]);
}

#[tokio::test]
async fn files_003_a_file_whose_extension_or_bytes_are_of_no_accepted_kind_is_refused() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let page = upload(&api, &ada, &workspace, "page.html", b"<script>", None).await;
    let disguised = upload(&api, &ada, &workspace, "photo.png", b"<script>", None).await;
    let bare = upload(&api, &ada, &workspace, "notes", b"hello", None).await;
    let binary = upload(&api, &ada, &workspace, "notes.txt", b"a\x00b", None).await;
    let office = upload(
        &api,
        &ada,
        &workspace,
        "report.docx",
        &archive(&["word/document.xml"]),
        None,
    )
    .await;

    for refused in [&page, &disguised, &bare, &binary, &office] {
        assert_eq!(refused.status, 422);
        assert_eq!(refused.json()["code"], "files.unsupported_type");
        assert_eq!(
            refused.json()["detail"],
            "The file's extension must be one the API accepts, and its contents of that type."
        );
    }
    assert!(stored_names(&api, &workspace).await.is_empty());
    assert!(objects(&api).await.is_empty());
}

#[tokio::test]
async fn files_004_a_body_over_the_limit_answers_413_and_an_empty_one_400() {
    let api = Api::with(Options::default().set("FILE_MAX_BYTES", 10)).await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let fits = upload(&api, &ada, &workspace, "a.txt", b"0123456789", None).await;
    let over = upload(&api, &ada, &workspace, "b.txt", b"0123456789A", None).await;
    let empty = upload(&api, &ada, &workspace, "c.txt", b"", None).await;

    assert_eq!(fits.status, 201);
    assert_eq!(over.status, 413);
    assert_eq!(over.json()["code"], "request.payload_too_large");
    assert_eq!(empty.status, 400);
    assert_eq!(empty.json()["code"], "files.empty");
    assert_eq!(stored_names(&api, &workspace).await, ["a.txt"]);
}

#[tokio::test]
async fn files_004_by_default_a_body_holds_at_most_10_485_760_bytes() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let most = 10_485_760;

    let at = upload(&api, &ada, &workspace, "a.txt", &vec![b'a'; most], None).await;
    let over = upload(&api, &ada, &workspace, "b.txt", &vec![b'a'; most + 1], None).await;

    assert_eq!(at.status, 201);
    assert_eq!(over.status, 413);
    assert_eq!(over.json()["code"], "request.payload_too_large");
}

/// How many uploads ended as `failed`, in every test that runs alongside.
fn failed_uploads() -> f64 {
    testing::recorded("files.uploads")
        .into_iter()
        .filter(|point| point.labels.get("files.outcome").map(String::as_str) == Some("failed"))
        .map(|point| point.value)
        .sum()
}

/// The uploads that `collector` received the count of, from a `serve` that has stopped, by how
/// each ended, such as `stored 1` (TEST-07).
fn uploads_counted(collector: &Collector) -> Vec<String> {
    counts::of(&collector.received(), "files.uploads", "files.outcome")
}

/// Uploads `bytes` as the file `name` to `workspace` as `user`, through `server`, with `key` as
/// its idempotency key if given, and gives the answer's status and body.
pub(crate) async fn upload_to(
    server: &Server,
    user: &SignedIn,
    workspace: &str,
    name: &str,
    bytes: &[u8],
    key: Option<&str>,
) -> (u16, Value) {
    let bearer = format!("Bearer {}", user.access);
    let disposition = format!("attachment; filename=\"{name}\"");
    let mut headers = vec![
        ("authorization", bearer.as_str()),
        ("content-type", "application/octet-stream"),
        ("content-disposition", &disposition),
    ];
    headers.extend(key.map(|key| ("idempotency-key", key)));
    let path = format!("/v1/workspaces/{workspace}/files");
    server.send("POST", &path, &headers, bytes).await
}

#[tokio::test]
async fn files_005_an_upload_in_which_the_scanner_finds_a_signature_is_refused() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let response = upload(&api, &ada, &workspace, "eicar.txt", &scanner::eicar(), None).await;

    assert_eq!(response.status, 422);
    assert_eq!(response.json()["code"], "files.malware_detected");
    assert_eq!(
        response.json()["detail"],
        "The malware scanner refused the file, so it wasn't stored."
    );
    assert!(stored_names(&api, &workspace).await.is_empty());
    assert!(objects(&api).await.is_empty());
    assert!(entries(&api, "file.uploaded").await.is_empty());
}

#[tokio::test]
async fn files_005_a_document_whose_archives_nest_past_the_scanner_s_limit_is_refused() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    // clamd scans archives nested 17 deep at most (the files spec's "Scanning").
    let mut nested = archive_of(&[("a.txt", b"hello")]);
    for _ in 0..20 {
        nested = archive_of(&[("inner.zip", &nested)]);
    }
    let document = archive_of(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("word/document.xml", b"<w:document/>"),
        ("word/embeddings/nested.zip", &nested),
    ]);
    let shallow = archive_of(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("word/document.xml", b"<w:document/>"),
        (
            "word/embeddings/one.zip",
            &archive_of(&[("a.txt", b"hello")]),
        ),
    ]);

    let refused = upload(&api, &ada, &workspace, "deep.docx", &document, None).await;
    let stored = upload(&api, &ada, &workspace, "shallow.docx", &shallow, None).await;

    assert_eq!(refused.status, 422);
    assert_eq!(refused.json()["code"], "files.malware_detected");
    assert_eq!(stored.status, 201, "{:?}", stored.json());
    assert_eq!(stored_names(&api, &workspace).await, ["shallow.docx"]);
}

#[tokio::test]
async fn files_005_while_the_scanner_doesn_t_answer_an_upload_answers_503_and_stores_nothing() {
    // Nothing listens on port 9 of the loopback address.
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", "127.0.0.1:9")).await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let response = upload(&api, &ada, &workspace, "a.txt", b"hello", None).await;

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "files.scanner_unavailable");
    assert_eq!(response.header("retry-after"), "1");
    assert!(stored_names(&api, &workspace).await.is_empty());
    assert!(objects(&api).await.is_empty());
    let errors = errors_of(&response);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].to_string().contains("scanner"), "{errors:#?}");
}

#[tokio::test]
async fn files_005_a_scanner_that_doesn_t_answer_within_its_time_answers_503() {
    let stand_in = scanner::scripted(Script::Never).await;
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", &stand_in.address)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let clock = api.clock();
    // A scan has 30 seconds (the platform spec's Scanner).
    let wait = Duration::from_secs(30);

    let (response, ()) = tokio::join!(
        upload(&api, &ada, &workspace, "a.txt", b"hello", None),
        async {
            stand_in.read.notified().await;
            clock.sleeping(wait).await;
            clock.advance(wait);
        }
    );

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "files.scanner_unavailable");
    assert!(stored_names(&api, &workspace).await.is_empty());
}

#[tokio::test]
async fn files_005_a_scan_that_fails_otherwise_answers_500() {
    let stand_in = scanner::scripted(Script::Answer("Can't allocate memory ERROR")).await;
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", &stand_in.address)).await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let response = upload(&api, &ada, &workspace, "a.txt", b"hello", None).await;

    assert_eq!(response.status, 500);
    assert_eq!(response.json()["code"], "request.internal_error");
    assert!(stored_names(&api, &workspace).await.is_empty());
    let errors = errors_of(&response);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(
        errors[0].to_string().contains("allocate memory"),
        "{errors:#?}"
    );
}

#[tokio::test]
async fn files_006_an_upload_refuses_a_caller_before_reading_the_body() {
    let api = Api::new().await;
    let (workspace, _, _) = acme(&api, "member").await;
    let eve = users::register(&api, "eve@example.com").await;

    // A body that fails once it is read, and a request without the name, which is refused before
    // the body is read: the refusal of the caller comes first.
    let path = format!("/v1/workspaces/{workspace}/files");
    let failing = upload_streamed(&api, path, &eve.access, unread())
        .await
        .unwrap();
    let unnamed = upload_with(&api, &eve, &workspace, &[], b"x", None).await;

    for refused in [&failing, &unnamed] {
        assert_eq!(refused.status, 404);
        assert_eq!(refused.json()["code"], "request.not_found");
    }
}

#[tokio::test]
async fn platform_073_a_check_refuses_and_records_as_opening_the_unit_of_work_does() {
    let api = Api::new().await;
    let (workspace, _, _) = acme(&api, "member").await;
    let eve = users::register(&api, "eve@example.com").await;

    let refused = upload(&api, &eve, &workspace, "a.txt", b"x", None).await;

    assert_eq!(refused.status, 404);
    // Recorded once, in Eve's own log, as any refusal of a non-member is (AUDIT-004).
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT scope || ' ' || user_id || ' ' || resource_id || ' ' \
                 || (details ->> 'permission') FROM audit_entries WHERE action = 'access.denied'"
            )
            .await,
        [format!("user {} {workspace} files.upload", eve.user_id)]
    );
    assert!(stored_names(&api, &workspace).await.is_empty());
}

#[tokio::test]
async fn platform_074_an_upload_takes_only_application_octet_stream_and_a_valid_key() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let path = format!("/v1/workspaces/{workspace}/files");

    let typed = api
        .request(http::Method::POST, &path)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=\"a.txt\"")
        .body(b"hello".to_vec())
        .bearer(&ada.access)
        .send()
        .await;
    let keyed = upload(&api, &ada, &workspace, "a.txt", b"hello", Some("not a key")).await;

    assert_eq!(typed.status, 400);
    assert_eq!(typed.json()["code"], "request.invalid_body");
    assert_eq!(keyed.status, 400);
    assert_eq!(keyed.json()["code"], "request.invalid_idempotency_key");
    assert!(stored_names(&api, &workspace).await.is_empty());
}

/// A body that the test feeds: `first`, once the API asks for it, which `reading` then tells;
/// then its end, once `go` is notified, or, without `go`, nothing more, ever.
fn fed(first: &'static [u8], reading: Arc<Notify>, go: Option<Arc<Notify>>) -> Body {
    Body::from_stream(stream::unfold(true, move |first_chunk| {
        let (reading, go) = (reading.clone(), go.clone());
        async move {
            if first_chunk {
                reading.notify_one();
                return Some((Ok::<_, std::io::Error>(Bytes::from_static(first)), false));
            }
            match go {
                Some(go) => go.notified().await,
                None => std::future::pending().await,
            }
            None
        }
    }))
}

/// An upload of `body` as `a.txt` to `path` as `token`, sent from a task of its own.
fn upload_streamed(
    api: &Api,
    path: String,
    token: &str,
    body: Body,
) -> tokio::task::JoinHandle<Response> {
    let (api, token) = (api.clone(), token.to_owned());
    tokio::spawn(async move {
        api.request(http::Method::POST, &path)
            .header("content-type", "application/octet-stream")
            .header("content-disposition", "attachment; filename=\"a.txt\"")
            .bearer(&token)
            .send_streamed(body)
            .await
    })
}

/// The answer to an upload whose `Content-Length` declares 11 bytes, over a limit of 10, with a
/// body that fails once it is read, so a 413 shows that nothing read it.
async fn declared_over_the_limit() -> Response {
    let api = Api::with(Options::default().set("FILE_MAX_BYTES", 10)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    api.request(
        http::Method::POST,
        &format!("/v1/workspaces/{workspace}/files"),
    )
    .header("content-type", "application/octet-stream")
    .header("content-disposition", "attachment; filename=\"a.txt\"")
    .header("content-length", "11")
    .bearer(&ada.access)
    .send_streamed(unread())
    .await
}

#[tokio::test]
async fn platform_074_a_declared_length_over_the_limit_answers_413_before_the_body_is_read() {
    let response = declared_over_the_limit().await;

    assert_eq!(response.status, 413);
    assert_eq!(response.json()["code"], "request.payload_too_large");
}

#[tokio::test]
async fn files_004_a_declared_length_over_the_limit_answers_413() {
    let response = declared_over_the_limit().await;

    assert_eq!(response.status, 413);
    assert_eq!(response.json()["code"], "request.payload_too_large");
}

#[tokio::test]
async fn platform_074_a_body_that_grows_past_the_limit_answers_413() {
    let api = Api::with(Options::default().set("FILE_MAX_BYTES", 10)).await;
    let (workspace, ada, _) = acme(&api, "member").await;

    // The in-process client declares no length, so the limit stops the body as it is read.
    let over = upload(&api, &ada, &workspace, "a.txt", b"0123456789A", None).await;

    assert_eq!(over.status, 413);
    assert_eq!(over.json()["code"], "request.payload_too_large");
    assert!(stored_names(&api, &workspace).await.is_empty());
}

#[tokio::test]
async fn platform_074_a_key_s_fingerprint_covers_the_bytes_and_the_name() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let first = upload(&api, &ada, &workspace, "a.txt", b"one", Some("\"k1\"")).await;
    let other_bytes = upload(&api, &ada, &workspace, "a.txt", b"two", Some("\"k1\"")).await;
    let other_name = upload(&api, &ada, &workspace, "b.txt", b"one", Some("\"k1\"")).await;

    assert_eq!(first.status, 201);
    for reused in [&other_bytes, &other_name] {
        assert_eq!(reused.status, 422);
        assert_eq!(reused.json()["code"], "request.idempotency_key_reused");
    }
}

#[tokio::test]
async fn platform_074_an_upload_past_its_timeout_answers_503() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let reading = Arc::new(Notify::new());
    let path = format!("/v1/workspaces/{workspace}/files");
    let request = upload_streamed(&api, path, &ada.access, fed(b"hel", reading.clone(), None));
    reading.notified().await;

    // Paused once the handler reads the body, after its check in the database, then moved on by
    // the route's 60 seconds, past its timeout, which started before the pause. The test yields,
    // rather than waiting, so the runtime moves the clock no further: under a longer timeout, the
    // request would wait for the rest of its body, which the test holds back.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    while !request.is_finished() {
        tokio::task::yield_now().await;
    }
    let response = request.await.unwrap();

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "request.timed_out");
}

/// The answer to Grace's upload, a member's, whose membership ends once her check has passed,
/// while its body is read, and the names the workspace's files then hold.
async fn left_while_uploading() -> (Response, Vec<String>) {
    let api = Api::new().await;
    let (workspace, _, grace) = acme(&api, "member").await;
    let (reading, go) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let path = format!("/v1/workspaces/{workspace}/files");
    let body = fed(b"hello", reading.clone(), Some(go.clone()));
    let request = upload_streamed(&api, path, &grace.access, body);
    reading.notified().await;

    api.database()
        .owner_executes(&format!(
            "DELETE FROM workspace_memberships \
             WHERE workspace_id = '{workspace}' AND user_id = '{}'",
            grace.user_id
        ))
        .await;
    go.notify_one();
    let response = request.await.unwrap();
    (response, stored_names(&api, &workspace).await)
}

#[tokio::test]
async fn platform_073_the_upload_s_unit_of_work_authorizes_the_caller_again() {
    let (response, stored) = left_while_uploading().await;

    assert_eq!(response.status, 404);
    assert_eq!(response.json()["code"], "request.not_found");
    assert!(stored.is_empty());
}

#[tokio::test]
async fn files_006_the_upload_s_unit_of_work_authorizes_the_caller_again() {
    let (response, stored) = left_while_uploading().await;

    assert_eq!(response.status, 404);
    assert!(stored.is_empty());
}

#[tokio::test]
async fn platform_073_a_check_refuses_a_member_without_the_permission_with_403() {
    let api = Api::new().await;
    let (workspace, _, grace) = acme(&api, "member").await;

    // The picture's upload asks for a permission that a member lacks, as a file's doesn't.
    let response = api
        .request(
            http::Method::PUT,
            &format!("/v1/workspaces/{workspace}/picture"),
        )
        .header("content-type", "application/octet-stream")
        .bearer(&grace.access)
        .send_streamed(unread())
        .await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "request.permission_denied");
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
                 || (details ->> 'permission') FROM audit_entries WHERE action = 'access.denied'"
            )
            .await,
        [format!(
            "workspace {workspace} {} workspace.update",
            grace.user_id
        )]
    );
}

#[tokio::test]
async fn files_007_lists_the_workspace_s_files_the_latest_uploaded_first() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    let db = api.database();
    let other = workspaces::arrange(db, "Other", &[(&ada.user_id, "owner")]).await;
    let first = files::arrange(db, &workspace, "First").await;
    let second = files::arrange(db, &workspace, "Second").await;
    files::arrange(db, &other, "Elsewhere").await;
    db.owner_executes(&format!(
        "UPDATE workspace_files SET created_at = CASE id \
         WHEN '{first}' THEN timestamptz '2026-11-01T10:00:00Z' \
         ELSE timestamptz '2026-11-01T11:00:00.5Z' END"
    ))
    .await;

    let response = api
        .get(&format!("/v1/workspaces/{workspace}/files"))
        .bearer(&grace.access)
        .send()
        .await;

    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(field(&body, "id"), [second, first]);
    assert_eq!(field(&body, "name"), ["Second", "First"]);
    assert_eq!(
        field(&body, "media_type"),
        ["text/plain; charset=utf-8", "text/plain; charset=utf-8"]
    );
    assert_eq!(
        field(&body, "created_at"),
        ["2026-11-01T11:00:00.500000Z", "2026-11-01T10:00:00.000000Z"]
    );
    assert_eq!(body["items"][0]["size_bytes"], 5);
    assert_eq!(body["items"][0]["uploader_id"], Value::Null);
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn files_007_paging_returns_each_file_once_across_a_shared_upload_time() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    let mut arranged = Vec::new();
    for name in ["A", "B", "C", "D", "E"] {
        arranged.push(files::arrange(db, &workspace, name).await);
    }
    db.owner_executes("UPDATE workspace_files SET created_at = '2026-11-01T10:00:00Z'")
        .await;

    let path = format!("/v1/workspaces/{workspace}/files?limit=2");
    let pages = pages(&api, &ada, &path, "id").await;

    // Among files that share an upload time, the higher ID comes first.
    arranged.sort();
    arranged.reverse();
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2, 1]);
    assert_eq!(pages.concat(), arranged);
}

#[tokio::test]
async fn files_007_a_search_pages_through_its_matches() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    for name in ["Blue one", "Red", "Blue two", "Blue three"] {
        files::arrange(db, &workspace, name).await;
    }

    let path = format!("/v1/workspaces/{workspace}/files?search=BLUE&limit=2");
    let pages = pages(&api, &ada, &path, "name").await;

    assert_eq!(pages, [vec!["Blue three", "Blue two"], vec!["Blue one"]]);
}

#[tokio::test]
async fn files_008_reading_a_file_gives_it() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    let uploaded = upload(&api, &ada, &workspace, "notes.md", b"# Notes\n", None)
        .await
        .json();
    let id = uploaded["id"].as_str().unwrap();

    let read = api
        .get(&format!("/v1/workspaces/{workspace}/files/{id}"))
        .bearer(&grace.access)
        .send()
        .await;

    assert_eq!(read.status, 200);
    // The links are signed for each caller's session (FILES-021).
    let read = read.json();
    assert_eq!(without_links(&read), without_links(&uploaded));
    let path = format!("/v1/workspaces/{workspace}/files/{id}/content?");
    assert!(read["content_url"].as_str().unwrap().starts_with(&path));
    assert_ne!(read["content_url"], uploaded["content_url"]);
}

#[tokio::test]
async fn files_008_an_id_that_names_no_file_of_the_workspace_answers_404() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    let other = workspaces::arrange(db, "Other", &[(&ada.user_id, "owner")]).await;
    let elsewhere = files::arrange(db, &other, "Elsewhere").await;

    for file in [
        elsewhere.as_str(),
        "019a0000-0000-7000-8000-000000000000",
        "not-a-uuid",
    ] {
        let path = format!("/v1/workspaces/{workspace}/files/{file}");
        let read = api.get(&path).bearer(&ada.access).send().await;
        let downloaded = download(&api, &ada, &workspace, file).await;
        let deleted = delete(&api, &ada, &workspace, file).await;

        for response in [read, downloaded, deleted] {
            assert_eq!(response.status, 404, "{file}");
            assert_eq!(response.json()["code"], "files.not_found", "{file}");
        }
    }
    assert_eq!(stored_names(&api, &other).await, ["Elsewhere"]);
}

/// A file of every accepted kind: its name, its bytes, the media type it is served as, and how a
/// browser is to show it (FILES-003, FILES-009).
fn every_kind() -> Vec<(&'static str, Vec<u8>, &'static str, &'static str)> {
    vec![
        (
            "a.png",
            b"\x89PNG\r\n\x1a\nrest".to_vec(),
            "image/png",
            "inline",
        ),
        (
            "b.JPG",
            b"\xff\xd8\xff\xe0rest".to_vec(),
            "image/jpeg",
            "inline",
        ),
        ("c.gif", b"GIF89a rest".to_vec(), "image/gif", "inline"),
        (
            "d.webp",
            b"RIFF\0\0\0\0WEBPVP8 ".to_vec(),
            "image/webp",
            "inline",
        ),
        (
            "e.pdf",
            b"%PDF-1.7\n".to_vec(),
            "application/pdf",
            "attachment",
        ),
        (
            "f.docx",
            archive(&["[Content_Types].xml", "word/document.xml"]),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "attachment",
        ),
        (
            "g.xlsx",
            archive(&["[Content_Types].xml", "xl/workbook.xml"]),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "attachment",
        ),
        (
            "h.pptx",
            archive(&["[Content_Types].xml", "ppt/presentation.xml"]),
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "attachment",
        ),
        (
            "i.txt",
            b"plain\n".to_vec(),
            "text/plain; charset=utf-8",
            "attachment",
        ),
        (
            "j.csv",
            b"a,b\n".to_vec(),
            "text/csv; charset=utf-8",
            "attachment",
        ),
        (
            "k.md",
            b"# k\n".to_vec(),
            "text/markdown; charset=utf-8",
            "attachment",
        ),
    ]
}

#[tokio::test]
async fn files_009_a_download_serves_each_kind_so_that_it_can_t_run() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    let kinds = every_kind();

    for (name, bytes, media_type, disposition) in kinds {
        let uploaded = upload(&api, &ada, &workspace, name, &bytes, None).await;
        assert_eq!(uploaded.status, 201, "{name}");
        let id = uploaded.json()["id"].as_str().unwrap().to_owned();

        let response = download(&api, &grace, &workspace, &id).await;

        assert_eq!(response.status, 200, "{name}");
        assert_eq!(response.body, bytes, "{name}");
        assert_eq!(response.header("content-type"), media_type, "{name}");
        assert_eq!(response.header("content-length"), bytes.len().to_string());
        assert_eq!(
            response.header("content-disposition"),
            format!("{disposition}; filename=\"{name}\"; filename*=UTF-8''{name}")
        );
        assert_eq!(response.header("x-content-type-options"), "nosniff");
        assert_eq!(
            response.header("content-security-policy"),
            "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; sandbox"
        );
        assert_eq!(response.header("cache-control"), "no-store");
    }
}

#[tokio::test]
async fn api_16_a_download_carries_its_trace_context() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange(api.database(), &workspace, "Blue").await;

    let response = download(&api, &ada, &workspace, &id).await;

    assert_eq!(response.status, 200);
    // `server-timing: trace;desc=00-<trace ID>-<span ID>-<flags>`.
    let traceparent = response.header("server-timing");
    let parts: Vec<&str> = traceparent["trace;desc=".len()..].split('-').collect();
    assert!(traceparent.starts_with("trace;desc=00-"), "{traceparent}");
    assert_eq!((parts[1].len(), parts[2].len()), (32, 16), "{traceparent}");
}

#[tokio::test]
async fn files_009_a_file_whose_bytes_the_bucket_lacks_answers_500_and_its_cause_names_it() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange(api.database(), &workspace, "Blue").await;
    let key = object_key(&api, &id).await;
    bucket::storage(&api.database().bucket().await)
        .delete(&ObjectKey::from_stored(&key))
        .await
        .unwrap();

    let response = download(&api, &ada, &workspace, &id).await;

    assert_eq!(response.status, 500);
    assert_eq!(response.json()["code"], "request.internal_error");
    let errors = errors_of(&response);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].to_string().contains(&id), "{errors:#?}");
}

#[tokio::test]
async fn files_009_a_file_whose_object_has_another_size_answers_500_and_its_cause_names_it() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;
    cut_short(&api, &object_key(&api, &id).await).await;

    let response = download(&api, &ada, &workspace, &id).await;

    assert_eq!(response.status, 500);
    assert_eq!(response.json()["code"], "request.internal_error");
    let errors = errors_of(&response);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].to_string().contains(&id), "{errors:#?}");
}

#[tokio::test]
async fn files_009_a_range_past_a_cut_short_object_s_end_answers_500_and_its_cause_names_it() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;
    let key = object_key(&api, &id).await;
    cut_short(&api, &key).await;
    // A resume from the byte that was cut.
    let short = files::BYTES.len() - 1;

    let path = format!("/v1/workspaces/{workspace}/files/{id}/content");
    let response =
        ranges::ranged(&api, &ada, &path, &[("range", &format!("bytes={short}-"))]).await;

    assert_eq!(response.status, 500);
    assert_eq!(response.json()["code"], "request.internal_error");
    let errors = errors_of(&response);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].to_string().contains(&id), "{errors:#?}");
    assert!(!errors[0].to_string().contains(&key), "{errors:#?}");
}

#[tokio::test]
async fn files_010_deleting_a_file_deletes_its_row_records_it_and_publishes_it() {
    let api = Api::new().await;
    let (workspace, _, grace) = acme(&api, "admin").await;
    let id = files::arrange(api.database(), &workspace, "Blue").await;
    let key = object_key(&api, &id).await;

    let response = delete(&api, &grace, &workspace, &id).await;

    assert_eq!(response.status, 204);
    assert!(stored_names(&api, &workspace).await.is_empty());
    // Its bytes stay until FILES-011 deletes them.
    assert_eq!(objects(&api).await, [key]);
    assert_eq!(
        entries(&api, "file.deleted").await,
        [format!(
            "workspace {workspace} {} file {id} {{}} files.read",
            grace.user_id
        )]
    );
    assert_eq!(
        published(&api, "file.deleted").await,
        [format!(
            "workspace {workspace} files.read {workspace} {id} 2"
        )]
    );
}

#[tokio::test]
async fn files_010_a_member_may_not_delete_and_the_owner_may() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    let id = files::arrange(api.database(), &workspace, "Grace's").await;

    let refused = delete(&api, &grace, &workspace, &id).await;
    let unchanged = stored_names(&api, &workspace).await;
    let deleted = delete(&api, &ada, &workspace, &id).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.permission_denied");
    assert_eq!(unchanged, ["Grace's"]);
    assert_eq!(deleted.status, 204);
    assert!(stored_names(&api, &workspace).await.is_empty());
}

#[tokio::test]
async fn files_011_the_comparison_records_no_object_that_an_export_holds() {
    // The bucket lists an object written long ago, of a user's file that an export of theirs
    // holds, and which they have since deleted.
    let bucket = bucket::listing(&[("users/held", "2020-01-01T00:00:00.000Z")]).await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    db.owner_executes(&format!(
        "INSERT INTO user_files \
         (user_id, name, media_type, size_bytes, crc32, visibility, object_key) \
         VALUES ('{}', 'Held', 'text/plain; charset=utf-8', 5, 0, 'private', 'users/held')",
        ada.user_id
    ))
    .await;
    crate::accounts::exports::built(&api, &ada).await;
    db.owner_executes("DELETE FROM user_files").await;
    assert_eq!(
        db.owner_reads("SELECT object_key FROM exported_files")
            .await,
        ["users/held"]
    );

    api.run_periodic("files.find_unreferenced_objects").await;

    assert!(
        db.owner_reads("SELECT object_key FROM object_deletions")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn files_011_the_comparison_records_each_object_no_row_names_written_an_hour_ago() {
    // The bucket lists five objects: four written long ago, two under `workspaces/` and two
    // under `users/`, of which a workspace's row names one and a user's row one, and one written
    // later than the database's clock reads now, as one whose upload may still commit.
    let bucket = bucket::listing(&[
        ("workspaces/settled", "2020-01-01T00:00:00.000Z"),
        ("workspaces/named", "2020-01-01T00:00:00.000Z"),
        ("users/settled", "2020-01-01T00:00:00.000Z"),
        ("users/named", "2020-01-01T00:00:00.000Z"),
        ("workspaces/recent", "2999-01-01T00:00:00.000Z"),
    ])
    .await;
    let api = Api::with(
        Options::default()
            .set("S3_ENDPOINT", &bucket.endpoint)
            .set("FILE_DELETION_DELAY_DAYS", 3),
    )
    .await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    db.owner_executes(&format!(
        "INSERT INTO workspace_files \
         (workspace_id, name, media_type, size_bytes, crc32, visibility, object_key) \
         VALUES ('{workspace}', 'Named', 'text/plain; charset=utf-8', 5, 0, 'private', \
         'workspaces/named')"
    ))
    .await;
    db.owner_executes(&format!(
        "INSERT INTO user_files \
         (user_id, name, media_type, size_bytes, crc32, visibility, object_key) \
         VALUES ('{}', 'Named', 'text/plain; charset=utf-8', 5, 0, 'private', 'users/named')",
        ada.user_id
    ))
    .await;

    api.run_periodic("files.find_unreferenced_objects").await;

    // Recorded 3 days from now, by the database's clock, and only the settled objects no row
    // names.
    assert_eq!(
        db.owner_reads(
            "SELECT object_key || ' ' || (deletable_at - now() BETWEEN interval '3 days' - \
             interval '1 minute' AND interval '3 days') FROM object_deletions \
             ORDER BY object_key"
        )
        .await,
        ["users/settled true", "workspaces/settled true"]
    );
    // A later run keeps the time it recorded.
    db.owner_executes("UPDATE object_deletions SET deletable_at = now() + interval '1 hour'")
        .await;
    api.run_periodic("files.find_unreferenced_objects").await;
    assert_eq!(
        db.owner_reads(
            "SELECT (deletable_at < now() + interval '2 hours')::text FROM object_deletions"
        )
        .await,
        ["true", "true"]
    );
}

#[tokio::test]
async fn files_011_by_default_an_object_goes_35_days_after_the_comparison_records_it() {
    let bucket = bucket::listing(&[("workspaces/settled", "2020-01-01T00:00:00.000Z")]).await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;

    api.run_periodic("files.find_unreferenced_objects").await;

    // By the database's clock.
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT (deletable_at - now() BETWEEN interval '35 days' - interval '1 minute' \
                 AND interval '35 days')::text FROM object_deletions"
            )
            .await,
        ["true"]
    );
}

#[tokio::test]
async fn files_011_the_comparison_records_an_object_written_over_an_hour_ago_and_none_within_it() {
    // The bucket lists two objects that no row names, written 61 and 59 minutes before now, by
    // the database's clock, which the comparison reads them by.
    let database = TestDatabase::migrated().await;
    let ago = |minutes: u32| {
        format!(
            "SELECT to_char((now() - interval '{minutes} minutes') AT TIME ZONE 'UTC', \
             'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"')"
        )
    };
    let over = database.owner_reads(&ago(61)).await.remove(0);
    let within = database.owner_reads(&ago(59)).await.remove(0);
    let bucket =
        bucket::listing(&[("workspaces/over", &over), ("workspaces/within", &within)]).await;
    let api = Api::on(
        database,
        Options::default().set("S3_ENDPOINT", &bucket.endpoint),
    )
    .await;

    api.run_periodic("files.find_unreferenced_objects").await;

    assert_eq!(
        api.database()
            .owner_reads("SELECT object_key FROM object_deletions")
            .await,
        ["workspaces/over"]
    );
}

#[tokio::test]
async fn files_011_an_unreferenced_object_goes_once_its_delay_has_passed() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    let kept = files::arrange(db, &workspace, "Kept").await;
    let kept = object_key(&api, &kept).await;
    let deleted = files::arrange(db, &workspace, "Deleted").await;
    let orphan = object_key(&api, &deleted).await;
    delete(&api, &ada, &workspace, &deleted).await;
    // The comparison recorded the object, with its time still to come.
    db.owner_executes(&format!(
        "INSERT INTO object_deletions (object_key, deletable_at) \
         VALUES ('{orphan}', now() + interval '1 hour')"
    ))
    .await;

    // Before its time, nothing goes.
    api.run_periodic("files.delete_unreferenced_objects").await;
    let mut both = vec![kept.clone(), orphan.clone()];
    both.sort();
    assert_eq!(objects(&api).await, both);

    db.owner_executes("UPDATE object_deletions SET deletable_at = now() - interval '1 second'")
        .await;
    api.run_periodic("files.delete_unreferenced_objects").await;

    assert_eq!(objects(&api).await, std::slice::from_ref(&kept));
    assert!(
        db.owner_reads("SELECT object_key FROM object_deletions")
            .await
            .is_empty()
    );
    // A second run changes nothing more (OPS-09).
    api.run_periodic("files.delete_unreferenced_objects").await;
    assert_eq!(objects(&api).await, [kept]);
}

#[tokio::test]
async fn files_011_a_run_deletes_more_recorded_objects_than_one_batch_holds() {
    let api = Api::new().await;
    let db = api.database();
    // One more than a batch's 1,000, whose time has passed, of objects that the bucket doesn't
    // hold, whose deletion succeeds as that of one a run before deleted does (OPS-09).
    db.owner_executes(
        "INSERT INTO object_deletions (object_key, deletable_at) \
         SELECT 'workspaces/' || md5(n::text), now() - interval '1 second' \
         FROM generate_series(1, 1001) AS n",
    )
    .await;

    api.run_periodic("files.delete_unreferenced_objects").await;

    assert!(
        db.owner_reads("SELECT object_key FROM object_deletions")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn ops_09_a_deletion_whose_object_a_run_before_deleted_succeeds() {
    let api = Api::new().await;
    let db = api.database();
    // A run deleted the object from the bucket, then ended before it removed the record.
    db.owner_executes(
        "INSERT INTO object_deletions (object_key, deletable_at) \
         VALUES ('workspaces/00000000000000000000000000000000', now() - interval '1 second')",
    )
    .await;

    // The run fails the test if it fails.
    api.run_periodic("files.delete_unreferenced_objects").await;

    assert!(
        db.owner_reads("SELECT object_key FROM object_deletions")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn files_011_an_object_that_a_row_names_again_stays() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    let id = files::arrange(db, &workspace, "Blue").await;
    let key = object_key(&api, &id).await;
    // The object is recorded while no row names it, then a row does, as after a restore.
    db.owner_executes(&format!(
        "INSERT INTO object_deletions (object_key, deletable_at) VALUES ('{key}', now() - interval '1 second')"
    ))
    .await;

    api.run_periodic("files.delete_unreferenced_objects").await;

    assert_eq!(objects(&api).await, [key]);
    assert!(
        db.owner_reads("SELECT object_key FROM object_deletions")
            .await
            .is_empty()
    );
    assert_eq!(download(&api, &ada, &workspace, &id).await.status, 200);
}

/// The lines that the request whose problem `response` is logged at `ERROR`.
fn errors_of(response: &Response) -> Vec<Value> {
    let trace_id = response.json()["trace_id"].as_str().unwrap().to_owned();
    testing::logged(&trace_id)
        .into_iter()
        .filter(|line| line["level"] == "ERROR")
        .collect()
}

#[tokio::test]
async fn files_012_a_bucket_that_doesn_t_answer_makes_the_routes_of_bytes_answer_503() {
    // Nothing listens on port 9 of the loopback address.
    let api = Api::with(Options::default().set("S3_ENDPOINT", "http://127.0.0.1:9")).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;
    let key = object_key(&api, &id).await;

    let uploaded = upload(&api, &ada, &workspace, "a.txt", b"hello", None).await;
    let downloaded = download(&api, &ada, &workspace, &id).await;

    for response in [&uploaded, &downloaded] {
        assert_eq!(response.status, 503);
        assert_eq!(response.json()["code"], "files.storage_unavailable");
        assert_eq!(response.header("retry-after"), "1");
    }
    assert_eq!(stored_names(&api, &workspace).await, ["Blue"]);
    // The cause the request log writes names no object's key (ERR-04).
    let errors = errors_of(&downloaded);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(!errors[0].to_string().contains(&key), "{errors:#?}");
}

#[tokio::test]
async fn files_012_each_call_that_the_bucket_doesn_t_answer_counts_once_as_unavailable() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;
    jobs::not_due(api.database()).await;
    let collector = Collector::start().await.unwrap();
    // Nothing listens on port 9 of the loopback address.
    let env = [("S3_ENDPOINT", "http://127.0.0.1:9")];
    let server = files::serving(api.database(), &collector, &env).await;
    let bearer = format!("Bearer {}", ada.access);
    let content = format!("/v1/workspaces/{workspace}/files/{id}/content");

    let (uploaded, _) = upload_to(&server, &ada, &workspace, "a.txt", b"hello", None).await;
    let (downloaded, _) = server
        .send("GET", &content, &[("authorization", &bearer)], b"")
        .await;
    server.stop().await;

    assert_eq!((uploaded, downloaded), (503, 503));
    assert_eq!(
        counts::of(
            &collector.received(),
            "platform.storage_requests",
            "storage.outcome"
        ),
        ["unavailable 2"]
    );
}

#[tokio::test]
async fn files_012_a_read_whose_answer_names_no_size_it_reads_counts_once_as_failed() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;
    jobs::not_due(api.database()).await;
    let collector = Collector::start().await.unwrap();
    // It answers a read of a range with the whole object, which names no size of the whole
    // that the client reads (PLATFORM-094).
    let bucket = bucket::giving().await;
    let env = [("S3_ENDPOINT", bucket.endpoint.as_str())];
    let server = files::serving(api.database(), &collector, &env).await;
    let bearer = format!("Bearer {}", ada.access);
    let headers = [("authorization", bearer.as_str()), ("range", "bytes=1-2")];
    let content = format!("/v1/workspaces/{workspace}/files/{id}/content");

    let (downloaded, _) = server.send("GET", &content, &headers, b"").await;
    server.stop().await;

    assert_eq!(downloaded, 500);
    assert_eq!(bucket.methods(), ["GET"]);
    assert_eq!(
        counts::of(
            &collector.received(),
            "platform.storage_requests",
            "storage.outcome"
        ),
        ["failed 1"]
    );
}

#[tokio::test]
async fn files_012_a_bucket_that_answers_it_can_t_serve_counts_as_not_answering() {
    for status in [500, 503, 429] {
        let bucket = bucket::scripted(Some(status)).await;
        let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
        let (workspace, ada, _) = acme(&api, "member").await;
        let id = files::arrange_row(api.database(), &workspace, "Blue").await;

        let uploaded = upload(&api, &ada, &workspace, "a.txt", b"hello", None).await;
        let downloaded = download(&api, &ada, &workspace, &id).await;

        for response in [&uploaded, &downloaded] {
            assert_eq!(response.status, 503, "{status}");
            assert_eq!(response.json()["code"], "files.storage_unavailable");
        }
    }
}

#[tokio::test]
async fn platform_094_a_call_reaches_the_bucket_once_whatever_answers_it() {
    for status in [503, 500, 429] {
        let bucket = bucket::scripted(Some(status)).await;
        let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
        let (workspace, ada, _) = acme(&api, "member").await;
        let id = files::arrange_row(api.database(), &workspace, "Blue").await;

        let downloaded = download(&api, &ada, &workspace, &id).await;

        assert_eq!(downloaded.status, 503, "{status}");
        assert_eq!(bucket.paths().len(), 1, "{status}: {:?}", bucket.paths());
    }
}

#[tokio::test]
async fn sec_10_the_bucket_s_client_follows_no_redirect() {
    let bucket = bucket::redirecting().await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;

    let downloaded = download(&api, &ada, &workspace, &id).await;

    // The redirect is an answer that the client can't read (FILES-012).
    assert_eq!(downloaded.status, 500);
    let paths = bucket.paths();
    assert!(!paths.is_empty());
    assert!(!paths.contains(&"/elsewhere".to_owned()), "{paths:?}");
}

#[tokio::test]
async fn obs_07_the_bucket_s_client_sends_no_trace_context_to_the_bucket() {
    let bucket = bucket::giving().await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;
    let path = format!("/v1/workspaces/{workspace}/files/{id}/content");
    // The download carries a trace context, which its call of the bucket runs in.
    let trace = "0af7651916cd43dd8448eb211c80319c";
    let traceparent = format!("00-{trace}-b7ad6b7169203331-01");
    let context = [
        ("traceparent", traceparent.as_str()),
        ("tracestate", "vendor=value"),
        ("baggage", "key=value"),
    ];

    let downloaded = ranges::ranged(&api, &ada, &path, &context).await;

    assert_eq!(downloaded.status, 200);
    assert_eq!(downloaded.trace_id(), trace);
    let headers = bucket.headers();
    assert_eq!(headers.len(), 1, "{headers:?}");
    assert!(headers[0].iter().any(|sent| sent == "host"), "{headers:?}");
    for (name, _) in context {
        assert!(!headers[0].iter().any(|sent| sent == name), "{headers:?}");
    }
}

#[test]
fn platform_076_the_client_refuses_to_start_without_its_authorities_file() {
    let config = |ca_path: Option<&str>| StorageConfig {
        endpoint: "https://s3.example.com".parse().unwrap(),
        region: "us-east-1".to_owned(),
        bucket: "files".to_owned(),
        access_key_id: "key".to_owned(),
        secret_access_key: Secret::new("secret".to_owned()),
        path_style: false,
        ca_path: ca_path.map(std::path::PathBuf::from),
    };
    let clock = Arc::new(TestClock::default());

    assert!(Storage::new(&config(None), clock.clone()).is_ok());
    assert!(Storage::new(&config(Some("/nonexistent/files-ca.crt")), clock).is_err());
}

#[tokio::test]
async fn files_012_a_bucket_that_never_answers_makes_the_routes_answer_503_in_time() {
    let bucket = bucket::scripted(None).await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;
    let clock = api.clock();
    // A download waits 4 seconds for the bucket's answer, within its route's 10, and an upload
    // 15, within its 60 (the platform spec's Storage).
    let waits = [Duration::from_secs(4), Duration::from_secs(15)];

    let (downloaded, ()) = tokio::join!(download(&api, &ada, &workspace, &id), async {
        clock.sleeping(waits[0]).await;
        clock.advance(waits[0]);
    });
    let (uploaded, ()) = tokio::join!(
        upload(&api, &ada, &workspace, "a.txt", b"hello", None),
        async {
            clock.sleeping(waits[1]).await;
            clock.advance(waits[1]);
        }
    );

    for response in [&uploaded, &downloaded] {
        assert_eq!(response.status, 503);
        assert_eq!(response.json()["code"], "files.storage_unavailable");
    }
}

#[tokio::test]
async fn files_012_a_request_that_the_bucket_refuses_answers_500_naming_no_key() {
    let bucket = bucket::scripted(Some(400)).await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;
    let key = object_key(&api, &id).await;

    let uploaded = upload(&api, &ada, &workspace, "a.txt", b"hello", None).await;
    let downloaded = download(&api, &ada, &workspace, &id).await;

    for response in [&uploaded, &downloaded] {
        assert_eq!(response.status, 500);
        assert_eq!(response.json()["code"], "request.internal_error");
    }
    let errors = errors_of(&downloaded);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].to_string().contains("400"), "{errors:#?}");
    assert!(!errors[0].to_string().contains(&key), "{errors:#?}");
}

#[tokio::test]
async fn files_013_uploads_are_counted_by_how_they_ended() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let collector = Collector::start().await.unwrap();
    let server = files::serving(api.database(), &collector, &[]).await;

    let eicar = scanner::eicar();
    for (name, bytes) in [
        ("a.txt", b"hello".as_slice()),
        ("a.png", b"hello"),
        ("eicar.txt", &eicar),
    ] {
        upload_to(&server, &ada, &workspace, name, bytes, None).await;
    }
    server.stop().await;

    assert_eq!(
        uploads_counted(&collector),
        ["malware_detected 1", "stored 1", "unsupported_type 1"]
    );
}

#[tokio::test]
async fn files_013_an_upload_refused_before_the_check_of_its_bytes_counts_under_no_outcome() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let collector = Collector::start().await.unwrap();
    let server = files::serving(api.database(), &collector, &[("FILE_MAX_BYTES", "10")]).await;

    // A name that FILES-002 refuses, an empty body and a body over the limit (FILES-004).
    let (unnamed, _) = upload_to(&server, &ada, &workspace, " ", b"hello", None).await;
    let (empty, _) = upload_to(&server, &ada, &workspace, "a.txt", b"", None).await;
    let (over, _) = upload_to(&server, &ada, &workspace, "a.txt", b"0123456789A", None).await;
    // One that reaches the check, so the count shows that the collector received it.
    let (stored, _) = upload_to(&server, &ada, &workspace, "a.txt", b"hello", None).await;
    server.stop().await;

    assert_eq!([unnamed, empty, over, stored], [400, 400, 413, 201]);
    assert_eq!(uploads_counted(&collector), ["stored 1"]);
}

#[tokio::test]
async fn files_013_a_retry_of_a_keyed_upload_counts_as_stored_too() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let collector = Collector::start().await.unwrap();
    let server = files::serving(api.database(), &collector, &[]).await;
    let (_, first) = upload_to(&server, &ada, &workspace, "a.txt", b"one", Some("\"k1\"")).await;

    let (status, retry) =
        upload_to(&server, &ada, &workspace, "a.txt", b"one", Some("\"k1\"")).await;
    server.stop().await;

    assert_eq!(status, 201);
    assert_eq!(retry["id"], first["id"]);
    assert_eq!(uploads_counted(&collector), ["stored 2"]);
}

#[tokio::test]
async fn files_013_an_upload_that_the_bucket_doesn_t_take_counts_as_failed() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let collector = Collector::start().await.unwrap();
    // Nothing listens on port 9 of the loopback address.
    let env = [("S3_ENDPOINT", "http://127.0.0.1:9")];
    let server = files::serving(api.database(), &collector, &env).await;

    let (status, _) = upload_to(&server, &ada, &workspace, "a.txt", b"hello", None).await;
    server.stop().await;

    assert_eq!(status, 503);
    assert_eq!(uploads_counted(&collector), ["failed 1"]);
}

#[tokio::test]
async fn files_013_an_upload_that_its_route_s_timeout_ends_counts_as_failed() {
    let stand_in = scanner::scripted(Script::Never).await;
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", &stand_in.address)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let before = failed_uploads();

    let (response, ()) = tokio::join!(
        upload(&api, &ada, &workspace, "a.txt", b"hello", None),
        async {
            stand_in.read.notified().await;
            // Paused once the scanner holds the scan, whose own wait runs on the test's clock,
            // so tokio's time runs straight to the route's timeout, the only timer left.
            tokio::time::pause();
        }
    );

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "request.timed_out");
    // The route's timeout runs on tokio's time, which a test pauses only for an API in its own
    // process (TEST-06), where tests running alongside upload too, so the count grows by at least
    // this one (TEST-07).
    assert!(failed_uploads() >= before + 1.0);
}

#[tokio::test]
async fn files_013_an_upload_whose_scanner_doesn_t_answer_counts_as_failed() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let collector = Collector::start().await.unwrap();
    // Nothing listens on port 9 of the loopback address.
    let env = [("CLAMD_ADDRESS", "127.0.0.1:9")];
    let server = files::serving(api.database(), &collector, &env).await;

    let (status, _) = upload_to(&server, &ada, &workspace, "a.txt", b"hello", None).await;
    server.stop().await;

    assert_eq!(status, 503);
    assert_eq!(uploads_counted(&collector), ["failed 1"]);
}

#[tokio::test]
async fn sec_19_file_events_reach_the_members_and_no_replay_from_before_joining() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    let alan = users::register(&api, "alan@example.com").await;
    let db = api.database();
    // An event to Alan alone gives his stream one to resume after.
    db.owner_executes(&events::statement(
        &[Event::to_user(&alan.user_id, "{}")],
        true,
    ))
    .await;
    let before = streams::opened(&api, &alan).await.last_event_id;
    // Every role holds files.read, which the file events require: no member lacks it.
    let mut owner = streams::opened(&api, &ada).await;
    let mut member = streams::opened(&api, &grace).await;

    let first = upload(&api, &ada, &workspace, "blue.txt", b"blue", None)
        .await
        .json();
    let first = first["id"].as_str().unwrap().to_owned();
    let deleted = delete(&api, &ada, &workspace, &first).await;
    workspaces::join(db, &workspace, &alan.user_id, "member").await;
    let mut joined = streams::connect(&api, &alan.access, before.as_deref()).await;
    joined.opened().await;
    let after = upload(&api, &ada, &workspace, "green.txt", b"green", None)
        .await
        .json();
    let after_joining = joined.event().await;

    assert_eq!(deleted.status, 204);
    let second = after["id"].as_str().unwrap();
    let expected = [
        ("file.uploaded", first.as_str()),
        ("file.deleted", first.as_str()),
        ("file.uploaded", second),
    ];
    let mut ids = Vec::new();
    for stream in [&mut owner, &mut member] {
        for (name, file) in expected {
            let event = stream.event().await;
            assert_eq!(event.event.as_deref(), Some(name));
            assert_eq!(
                event.payload(),
                &json!({ "workspace_id": workspace, "file_id": file })
            );
            ids.push(event.id);
        }
    }
    // Neither event from before Alan joined is replayed to him: the first he receives is the
    // upload made after.
    assert_eq!(after_joining.event.as_deref(), Some("file.uploaded"));
    assert_eq!(after_joining.id, ids[2]);
}
