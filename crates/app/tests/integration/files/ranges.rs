//! What a download serves of a file's bytes: a range of them, every byte, or nothing, as RFC 9110
//! defines it (PLATFORM-081), so that a download cut short resumes (OPS-06).

use serde_json::Value;

use super::{acme, download, upload_queried};
use crate::support::{Api, api::Response, users::SignedIn};

/// The bytes of the file the tests download.
const TEXT: &[u8] = b"hello world";

/// A text file of [`TEXT`] that `ada` uploaded to `workspace`, as `query` says, as the upload
/// answered it.
async fn uploaded(api: &Api, ada: &SignedIn, workspace: &str, query: &str) -> Value {
    upload_queried(api, ada, workspace, query, "a.txt", TEXT)
        .await
        .json()
}

/// The download of the file at `path` as `ada`, with `headers`.
async fn ranged(api: &Api, ada: &SignedIn, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut call = api.get(path).bearer(&ada.access);
    for (name, value) in headers {
        call = call.header(name, value);
    }
    call.send().await
}

#[tokio::test]
async fn platform_081_one_range_of_a_download_answers_206_with_those_bytes() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let private = uploaded(&api, &ada, &workspace, "").await;
    let public = uploaded(&api, &ada, &workspace, "?visibility=public").await;
    let id = private["id"].as_str().unwrap();
    let path = format!("/v1/workspaces/{workspace}/files/{id}/content");

    for (path, tag) in [
        (path.as_str(), id),
        (private["content_url"].as_str().unwrap(), id),
        (
            public["content_url"].as_str().unwrap(),
            public["id"].as_str().unwrap(),
        ),
    ] {
        let response = ranged(&api, &ada, path, &[("range", "bytes=6-")]).await;

        assert_eq!(response.status, 206, "{path}");
        assert_eq!(response.body, b"world", "{path}");
        assert_eq!(response.header("content-range"), "bytes 6-10/11");
        assert_eq!(response.header("content-length"), "5");
        assert_eq!(response.header("etag"), format!("\"{tag}\""));
        assert_eq!(response.header("accept-ranges"), "bytes");
        assert_eq!(response.header("content-type"), "text/plain; charset=utf-8");
        assert_eq!(
            response.header("content-security-policy"),
            "default-src 'none'; sandbox"
        );
        assert_eq!(response.header("x-content-type-options"), "nosniff");
        // A download sets no cache's lifetime of its own but a public one's, so the security
        // headers' no-store stands.
        let cache = if path.starts_with("/v1/public/") {
            "public, max-age=300"
        } else {
            "no-store"
        };
        assert_eq!(response.header("cache-control"), cache, "{path}");
    }
    let suffix = ranged(&api, &ada, &path, &[("range", "bytes=-5")]).await;
    assert_eq!(suffix.body, b"world");
    let middle = ranged(&api, &ada, &path, &[("range", "bytes=2-4")]).await;
    assert_eq!(middle.body, b"llo");
    assert_eq!(middle.header("content-range"), "bytes 2-4/11");
}

#[tokio::test]
async fn platform_081_a_range_it_can_t_serve_or_another_tag_s_answers_every_byte() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let id = file["id"].as_str().unwrap();
    let path = format!("/v1/workspaces/{workspace}/files/{id}/content");

    for headers in [
        vec![("range", "bytes=11-")],
        vec![("range", "bytes=0-1, 4-5")],
        vec![("range", "items=0-1")],
        vec![("range", "bytes=6-"), ("if-range", "\"another\"")],
    ] {
        let response = ranged(&api, &ada, &path, &headers).await;

        assert_eq!(response.status, 200, "{headers:?}");
        assert_eq!(response.body, TEXT, "{headers:?}");
        assert_eq!(response.header("content-length"), "11");
        assert!(response.headers.get("content-range").is_none());
    }
    let same = ranged(
        &api,
        &ada,
        &path,
        &[("range", "bytes=6-"), ("if-range", &format!("\"{id}\""))],
    )
    .await;
    assert_eq!(same.status, 206);
    assert_eq!(same.body, b"world");
}

#[tokio::test]
async fn platform_081_if_none_match_of_its_tag_answers_304_with_no_body() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let public = uploaded(&api, &ada, &workspace, "?visibility=public").await;
    let id = public["id"].as_str().unwrap();
    let url = public["content_url"].as_str().unwrap();

    let held = api
        .get(url)
        .header("if-none-match", &format!("W/\"{id}\""))
        .send()
        .await;
    let changed = api
        .get(url)
        .header("if-none-match", "\"another\"")
        .send()
        .await;

    assert_eq!(held.status, 304);
    assert!(held.body.is_empty());
    assert_eq!(held.header("etag"), format!("\"{id}\""));
    assert_eq!(held.header("cache-control"), "public, max-age=300");
    assert_eq!(changed.status, 200);
    assert_eq!(changed.body, TEXT);
}

#[tokio::test]
async fn platform_081_head_answers_as_get_does_without_the_bytes() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let path = format!(
        "/v1/workspaces/{workspace}/files/{}/content",
        file["id"].as_str().unwrap()
    );

    let head = api
        .request(http::Method::HEAD, &path)
        .header("range", "bytes=6-")
        .bearer(&ada.access)
        .send()
        .await;

    // A `Range` of a `HEAD` is ignored, as RFC 9110 requires.
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(head.header("content-length"), "11");
    assert_eq!(head.header("accept-ranges"), "bytes");
}

#[tokio::test]
async fn ops_06_a_download_cut_short_resumes_from_another_replica_with_a_range() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let id = file["id"].as_str().unwrap();
    let replica = api.replica().await;

    // The first replica sent the first 6 bytes when a deploy's drain cut its download short.
    let first = download(&api, &ada, &workspace, id).await;
    let received = &first.body[..6];
    let tag = first.header("etag").to_owned();
    let rest = ranged(
        &replica,
        &ada,
        &format!("/v1/workspaces/{workspace}/files/{id}/content"),
        &[("range", "bytes=6-"), ("if-range", &tag)],
    )
    .await;

    assert_eq!(rest.status, 206);
    assert_eq!([received, rest.body.as_slice()].concat(), TEXT);
}
