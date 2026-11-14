//! Workspaces' pictures: setting and removing one through `workspaces`, which `files` keeps, its
//! links, and its public download (WORKSPACES-019 to WORKSPACES-022, FILES-011, FILES-013,
//! FILES-027).

use std::time::Duration;

use axum::body::Body;
use bytes::Bytes;
use futures_util::stream;
use serde_json::json;

use super::{errors_of, objects, uploads};
use crate::support::{
    Api,
    api::{Options, Response},
    bucket, execute_on, queries_wait_on_locks,
    scanner::{self, Script},
    users::{self, SignedIn},
    workspaces,
};

/// The smallest PNG's bytes: its signature, then its header chunk.
const PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";

/// Ada, and a workspace that she owns.
async fn owned(api: &Api) -> (SignedIn, String) {
    let ada = users::register(api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    (ada, workspace)
}

/// Sets `bytes` as the picture of `workspace`, as `user`.
async fn set(api: &Api, user: &SignedIn, workspace: &str, bytes: &[u8]) -> Response {
    api.request(
        http::Method::PUT,
        &format!("/v1/workspaces/{workspace}/picture"),
    )
    .header("content-type", "application/octet-stream")
    .body(bytes.to_vec())
    .bearer(&user.access)
    .send()
    .await
}

/// Sets [`PNG`] as the picture of `workspace`, as `user`, and gives its link.
async fn link(api: &Api, user: &SignedIn, workspace: &str) -> String {
    let response = set(api, user, workspace, PNG).await;
    assert_eq!(response.status, 200);
    response.json()["picture_url"].as_str().unwrap().to_owned()
}

/// Removes the picture of `workspace`, as `user`.
async fn remove(api: &Api, user: &SignedIn, workspace: &str) -> Response {
    api.request(
        http::Method::DELETE,
        &format!("/v1/workspaces/{workspace}/picture"),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// A body that fails once it is read, so an answer to a request that sends it shows that nothing
/// read it.
fn unread() -> Body {
    Body::from_stream(stream::once(async {
        Err::<Bytes, _>(std::io::Error::other("the body was read"))
    }))
}

/// Reads `workspace` as `user`.
async fn read(api: &Api, user: &SignedIn, workspace: &str) -> Response {
    api.get(&format!("/v1/workspaces/{workspace}"))
        .bearer(&user.access)
        .send()
        .await
}

/// Downloads the picture whose link is `url`, without a credential.
async fn download(api: &Api, url: &str) -> Response {
    api.get(url).send().await
}

/// The pictures' rows, each as its workspace, media type and size.
async fn rows(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT workspace_id || ' ' || media_type || ' ' || size_bytes \
             FROM workspace_pictures ORDER BY id",
        )
        .await
}

/// The ID that a picture's link names.
fn id_of(url: &str) -> &str {
    url.strip_prefix("/v1/public/workspace-pictures/").unwrap()
}

/// The workspace's version, how many entries of `action` are recorded, and how many events are
/// published.
async fn recorded(api: &Api, workspace: &str, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT (SELECT version FROM workspaces WHERE id = '{workspace}') || ' ' \
             || (SELECT count(*) FROM audit_entries WHERE action = '{action}') || ' ' \
             || (SELECT count(*) FROM events)"
        ))
        .await
}

#[tokio::test]
async fn workspaces_019_setting_a_picture_stores_it_records_it_and_publishes_it() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    workspaces::join(db, &workspace, &grace.user_id, "admin").await;
    // The registrations' sessions published their starts (AUTH-046).
    db.owner_executes("DELETE FROM events").await;

    let response = set(&api, &grace, &workspace, PNG).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("etag"), "\"2\"");
    let body = response.json();
    assert_eq!(body["id"], workspace);
    assert_eq!(body["name"], "Acme");
    assert_eq!(body["role"], "admin");
    let url = body["picture_url"].as_str().unwrap().to_owned();
    let id = id_of(&url);
    assert_eq!(
        rows(&api).await,
        [format!("{workspace} image/png {}", PNG.len())]
    );
    let downloaded = download(&api, &url).await;
    assert_eq!(downloaded.status, 200);
    assert_eq!(downloaded.header("content-type"), "image/png");
    assert_eq!(downloaded.body, PNG);
    let entries = db
        .owner_reads(
            "SELECT scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
             || coalesce(target_user_id::text, '-') || ' ' || resource_type || ' ' \
             || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = 'picture.uploaded'",
        )
        .await;
    assert_eq!(
        entries,
        [format!(
            "workspace {workspace} {} - picture {id} {{}}",
            grace.user_id
        )]
    );
    let events = db
        .owner_reads(
            "SELECT name || ' ' || scope || ' ' || workspace_id || ' ' || permission || ' ' \
             || payload::text FROM events",
        )
        .await;
    assert_eq!(
        events,
        [format!(
            "workspace.updated workspace {workspace} workspace.read {{\"workspace_id\": \"{workspace}\"}}"
        )]
    );
    // The owner reads the same picture, at the version that the change left.
    let read = read(&api, &ada, &workspace).await;
    assert_eq!(read.json()["picture_url"], url);
    assert_eq!(read.header("etag"), "\"2\"");
}

#[tokio::test]
async fn workspaces_019_a_new_picture_replaces_the_one_before_whose_bytes_stay() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let first = link(&api, &ada, &workspace).await;
    let gif = b"GIF89a\x01\0\x01\0\0\0\0;";

    let second = set(&api, &ada, &workspace, gif).await;

    assert_eq!(second.status, 200);
    assert_eq!(second.header("etag"), "\"3\"");
    let url = second.json()["picture_url"].as_str().unwrap().to_owned();
    assert_ne!(url, first);
    assert_eq!(
        rows(&api).await,
        [format!("{workspace} image/gif {}", gif.len())]
    );
    assert_eq!(download(&api, &first).await.status, 404);
    assert_eq!(download(&api, &url).await.status, 200);
    // The replaced image's object stays until the deletion of unreferenced objects (FILES-011).
    assert_eq!(objects(&api).await.len(), 2);
}

#[tokio::test]
async fn workspaces_019_a_member_and_a_stranger_are_refused_with_the_body_unread() {
    let api = Api::new().await;
    let (_, workspace) = owned(&api).await;
    let member = users::register(&api, "grace@example.com").await;
    let stranger = users::register(&api, "alan@example.com").await;
    workspaces::join(api.database(), &workspace, &member.user_id, "member").await;

    let mut refused = Vec::new();
    for caller in [&member, &stranger] {
        let response = api
            .request(
                http::Method::PUT,
                &format!("/v1/workspaces/{workspace}/picture"),
            )
            .header("content-type", "application/octet-stream")
            .bearer(&caller.access)
            .send_streamed(unread())
            .await;
        refused.push((response.status.as_u16(), response.json()["code"].clone()));
    }

    assert_eq!(
        refused,
        [
            (403, json!("request.permission_denied")),
            (404, json!("request.not_found"))
        ]
    );
    assert!(rows(&api).await.is_empty());
    assert!(objects(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_019_setting_a_picture_with_a_stale_etag_answers_412() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let stale = read(&api, &ada, &workspace).await.header("etag").to_owned();
    let first = link(&api, &ada, &workspace).await;
    // The registration's session and the first change published their events.
    api.database().owner_executes("DELETE FROM events").await;

    let response = api
        .request(
            http::Method::PUT,
            &format!("/v1/workspaces/{workspace}/picture"),
        )
        .header("content-type", "application/octet-stream")
        .header("if-match", &stale)
        .body(PNG.to_vec())
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 412);
    assert_eq!(response.json()["code"], "request.precondition_failed");
    assert_eq!(
        read(&api, &ada, &workspace).await.json()["picture_url"],
        first
    );
    // Nothing more than the first change's version and entry, and no event.
    assert_eq!(
        recorded(&api, &workspace, "picture.uploaded").await,
        ["2 1 0"]
    );
}

#[tokio::test]
async fn data_20_a_change_of_a_picture_waits_for_another_and_replaces_what_it_set() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let db = api.database();
    // Another change of the picture, as far as its lock of the workspace's row.
    let mut other = db
        .holding(&format!(
            "UPDATE workspaces SET version = version WHERE id = '{workspace}'"
        ))
        .await;

    let request = tokio::spawn({
        let (api, workspace) = (api.clone(), workspace.clone());
        async move { set(&api, &ada, &workspace, PNG).await }
    });
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut other,
        &format!(
            "INSERT INTO workspace_pictures \
             (workspace_id, media_type, size_bytes, crc32, object_key) \
             VALUES ('{workspace}', 'image/gif', 5, 0, 'pictures/other')"
        ),
    )
    .await;
    execute_on(&mut other, "COMMIT").await;
    let response = request.await.unwrap();

    // The request waited at the workspace's row, so it replaced the picture that the other set,
    // where a change that held no lock would have met the row's unique workspace.
    assert_eq!(response.status, 200);
    assert_eq!(response.header("etag"), "\"3\"");
    assert_eq!(
        rows(&api).await,
        [format!("{workspace} image/png {}", PNG.len())]
    );
}

#[tokio::test]
async fn workspaces_020_a_picture_is_a_png_jpeg_gif_or_webp_image_its_kind_from_its_bytes() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let images: [(&[u8], &str); 4] = [
        (PNG, "image/png"),
        (b"\xff\xd8\xff\xe0\0\x10JFIF", "image/jpeg"),
        (b"GIF87a\x01\0\x01\0\0\0\0;", "image/gif"),
        (b"RIFF\x1a\0\0\0WEBPVP8 ", "image/webp"),
    ];

    for (bytes, media_type) in images {
        let response = set(&api, &ada, &workspace, bytes).await;

        assert_eq!(response.status, 200, "{media_type}");
        let url = response.json()["picture_url"].as_str().unwrap().to_owned();
        let downloaded = download(&api, &url).await;
        assert_eq!(downloaded.header("content-type"), media_type);
    }
}

#[tokio::test]
async fn workspaces_020_an_empty_body_or_one_that_is_no_such_image_answers_422() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let refused: [&[u8]; 4] = [b"", b"hello", b"%PDF-1.7", b"<svg></svg>"];

    for bytes in refused {
        let response = set(&api, &ada, &workspace, bytes).await;

        assert_eq!(response.status, 422, "{bytes:?}");
        assert_eq!(response.json()["code"], "workspaces.picture_not_image");
    }
    assert!(rows(&api).await.is_empty());
    assert!(objects(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_020_a_body_of_more_than_1_mib_answers_413() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let mut large = PNG.to_vec();
    large.resize(1_048_577, 0);
    let mut largest = PNG.to_vec();
    largest.resize(1_048_576, 0);

    let refused = set(&api, &ada, &workspace, &large).await;
    let accepted = set(&api, &ada, &workspace, &largest).await;

    assert_eq!(refused.status, 413);
    assert_eq!(refused.json()["code"], "request.payload_too_large");
    assert_eq!(accepted.status, 200);
    assert_eq!(rows(&api).await, [format!("{workspace} image/png 1048576")]);
}

#[tokio::test]
async fn workspaces_020_a_declared_length_over_1_mib_answers_413_before_the_body_is_read() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;

    let response = api
        .request(
            http::Method::PUT,
            &format!("/v1/workspaces/{workspace}/picture"),
        )
        .header("content-type", "application/octet-stream")
        .header("content-length", "1048577")
        .bearer(&ada.access)
        .send_streamed(unread())
        .await;

    assert_eq!(response.status, 413);
    assert_eq!(response.json()["code"], "request.payload_too_large");
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_020_a_picture_in_which_the_scanner_finds_malware_answers_422() {
    let stand_in = scanner::scripted(Script::Answer("stream: Eicar-Test-Signature FOUND")).await;
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", &stand_in.address)).await;
    let (ada, workspace) = owned(&api).await;

    let response = set(&api, &ada, &workspace, PNG).await;

    assert_eq!(response.status, 422);
    assert_eq!(
        response.json()["code"],
        "workspaces.picture_malware_detected"
    );
    assert!(rows(&api).await.is_empty());
    assert!(objects(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_020_while_the_scanner_or_the_storage_doesn_t_answer_a_picture_answers_503() {
    // Nothing listens on port 9 of the loopback address.
    let scanner_down = Api::with(Options::default().set("CLAMD_ADDRESS", "127.0.0.1:9")).await;
    let storage_down = Api::with(Options::default().set("S3_ENDPOINT", "http://127.0.0.1:9")).await;

    for api in [&scanner_down, &storage_down] {
        let (ada, workspace) = owned(api).await;
        let response = set(api, &ada, &workspace, PNG).await;

        assert_eq!(response.status, 503);
        assert_eq!(response.json()["code"], "workspaces.picture_unavailable");
        assert_eq!(response.header("retry-after"), "1");
        assert!(rows(api).await.is_empty());
        let errors = errors_of(&response);
        assert_eq!(errors.len(), 1, "{errors:#?}");
    }
}

#[tokio::test]
async fn workspaces_020_a_scanner_that_doesn_t_answer_within_its_time_answers_503() {
    let stand_in = scanner::scripted(Script::Never).await;
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", &stand_in.address)).await;
    let (ada, workspace) = owned(&api).await;
    let clock = api.clock();
    // A scan has 30 seconds (the platform spec's Scanner).
    let wait = Duration::from_secs(30);

    let (response, ()) = tokio::join!(set(&api, &ada, &workspace, PNG), async {
        stand_in.read.notified().await;
        clock.sleeping(wait).await;
        clock.advance(wait);
    });

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "workspaces.picture_unavailable");
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_020_while_the_storage_doesn_t_answer_within_its_time_a_picture_answers_503() {
    let bucket = bucket::scripted(None).await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
    let (ada, workspace) = owned(&api).await;
    let clock = api.clock();
    // An upload waits 15 seconds for the bucket's answer, within its route's 60 (the platform
    // spec's Storage).
    let wait = Duration::from_secs(15);

    let (response, ()) = tokio::join!(set(&api, &ada, &workspace, PNG), async {
        clock.sleeping(wait).await;
        clock.advance(wait);
    });

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "workspaces.picture_unavailable");
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_020_a_picture_that_its_route_s_timeout_ends_answers_503_timed_out() {
    let stand_in = scanner::scripted(Script::Never).await;
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", &stand_in.address)).await;
    let (ada, workspace) = owned(&api).await;

    let (response, ()) = tokio::join!(set(&api, &ada, &workspace, PNG), async {
        stand_in.read.notified().await;
        // Paused once the scanner holds the scan, whose own wait runs on the test's clock, so
        // tokio's time runs straight to the route's timeout, the only timer left.
        tokio::time::pause();
    });

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "request.timed_out");
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_021_removing_a_picture_deletes_it_records_it_and_publishes_it() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let url = link(&api, &ada, &workspace).await;
    let id = id_of(&url).to_owned();
    let db = api.database();
    db.owner_executes("DELETE FROM events").await;

    let response = remove(&api, &ada, &workspace).await;

    assert_eq!(response.status, 204);
    assert!(rows(&api).await.is_empty());
    assert_eq!(download(&api, &url).await.status, 404);
    let read = read(&api, &ada, &workspace).await;
    assert_eq!(read.json()["picture_url"], json!(null));
    assert_eq!(read.header("etag"), "\"3\"");
    let entries = db
        .owner_reads(
            "SELECT scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
             || resource_type || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = 'picture.deleted'",
        )
        .await;
    assert_eq!(
        entries,
        [format!(
            "workspace {workspace} {} picture {id} {{}}",
            ada.user_id
        )]
    );
    assert_eq!(
        db.owner_reads(
            "SELECT name || ' ' || workspace_id || ' ' || permission || ' ' || payload::text \
             FROM events"
        )
        .await,
        [format!(
            "workspace.updated {workspace} workspace.read {{\"workspace_id\": \"{workspace}\"}}"
        )]
    );
    // Its object stays until the deletion of unreferenced objects (FILES-011).
    assert_eq!(objects(&api).await.len(), 1);
}

#[tokio::test]
async fn workspaces_021_removing_no_picture_answers_204_and_changes_nothing() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    // The registration's session published its start (AUTH-046).
    api.database().owner_executes("DELETE FROM events").await;

    let response = remove(&api, &ada, &workspace).await;

    assert_eq!(response.status, 204);
    assert_eq!(
        recorded(&api, &workspace, "picture.deleted").await,
        ["1 0 0"]
    );
}

#[tokio::test]
async fn workspaces_021_removing_a_picture_with_a_stale_etag_answers_412_and_with_the_current_204()
{
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let stale = read(&api, &ada, &workspace).await.header("etag").to_owned();
    link(&api, &ada, &workspace).await;
    let current = read(&api, &ada, &workspace).await.header("etag").to_owned();
    let remove_with = |etag: &str| {
        api.request(
            http::Method::DELETE,
            &format!("/v1/workspaces/{workspace}/picture"),
        )
        .header("if-match", etag)
        .bearer(&ada.access)
    };

    let refused = remove_with(&stale).send().await;
    let kept = rows(&api).await.len();
    let removed = remove_with(&current).send().await;

    assert_eq!(refused.status, 412);
    assert_eq!(refused.json()["code"], "request.precondition_failed");
    assert_eq!(kept, 1);
    assert_eq!(removed.status, 204);
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_021_a_member_may_not_remove_the_picture_and_an_admin_may() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let member = users::register(&api, "grace@example.com").await;
    let admin = users::register(&api, "alan@example.com").await;
    let db = api.database();
    workspaces::join(db, &workspace, &member.user_id, "member").await;
    workspaces::join(db, &workspace, &admin.user_id, "admin").await;
    link(&api, &ada, &workspace).await;

    let refused = remove(&api, &member, &workspace).await;
    let kept = rows(&api).await.len();
    let removed = remove(&api, &admin, &workspace).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.permission_denied");
    assert_eq!(kept, 1);
    assert_eq!(removed.status, 204);
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn workspaces_022_a_workspace_carries_the_link_of_its_picture_or_null() {
    let api = Api::new().await;
    let (ada, acme) = owned(&api).await;
    let bare = workspaces::arrange(api.database(), "Bare", &[(&ada.user_id, "member")]).await;
    let before = read(&api, &ada, &acme).await;

    let url = link(&api, &ada, &acme).await;

    assert_eq!(before.json()["picture_url"], json!(null));
    assert_eq!(read(&api, &ada, &acme).await.json()["picture_url"], url);
    assert_eq!(
        read(&api, &ada, &bare).await.json()["picture_url"],
        json!(null)
    );
    let list = api.get("/v1/workspaces").bearer(&ada.access).send().await;
    let links: Vec<_> = list.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|workspace| (workspace["id"].clone(), workspace["picture_url"].clone()))
        .collect();
    assert_eq!(
        links,
        [(json!(bare), json!(null)), (json!(acme), json!(url))]
    );
}

#[tokio::test]
async fn files_027_a_picture_s_download_needs_no_credential_and_names_it_by_its_id() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let url = link(&api, &ada, &workspace).await;
    let id = id_of(&url);

    let response = download(&api, &url).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), "image/png");
    assert_eq!(response.body, PNG);
    assert_eq!(
        response.header("content-disposition"),
        format!("inline; filename=\"{id}.png\"; filename*=UTF-8''{id}.png")
    );
    assert_eq!(response.header("cache-control"), "public, max-age=300");
    assert_eq!(response.header("etag"), format!("\"{id}\""));
    let unknown = download(
        &api,
        "/v1/public/workspace-pictures/0198f1a2-0000-7000-8000-000000000000",
    )
    .await;
    let not_uuid = download(&api, "/v1/public/workspace-pictures/not-a-uuid").await;
    for missing in [&unknown, &not_uuid] {
        assert_eq!(missing.status, 404);
        assert_eq!(missing.json()["code"], "files.not_found");
    }
}

#[tokio::test]
async fn files_027_a_picture_s_download_serves_a_range_of_its_bytes() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let url = link(&api, &ada, &workspace).await;

    let response = api.get(&url).header("range", "bytes=0-7").send().await;

    assert_eq!(response.status, 206);
    assert_eq!(
        response.header("content-range"),
        format!("bytes 0-7/{}", PNG.len())
    );
    assert_eq!(response.body, &PNG[..8]);
}

#[tokio::test]
async fn files_027_a_picture_s_download_counts_in_the_bucket_of_public_downloads() {
    let api = Api::with(Options::default().set("RATE_LIMIT_PUBLIC_REQUESTS", 7)).await;
    let (ada, workspace) = owned(&api).await;
    let url = link(&api, &ada, &workspace).await;

    let response = download(&api, &url).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("ratelimit-policy"), "\"public\";q=7;w=60");
}

#[tokio::test]
async fn files_027_the_download_of_a_picture_replaced_removed_or_gone_with_its_workspace_answers_404()
 {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let db = api.database();
    let other = workspaces::arrange(db, "Other", &[(&ada.user_id, "owner")]).await;
    let replaced = link(&api, &ada, &workspace).await;
    let removed = link(&api, &ada, &workspace).await;
    remove(&api, &ada, &workspace).await;
    let gone = link(&api, &ada, &other).await;
    db.owner_executes(&format!("DELETE FROM workspaces WHERE id = '{other}'"))
        .await;

    for url in [&replaced, &removed, &gone] {
        let response = download(&api, url).await;

        assert_eq!(response.status, 404);
        assert_eq!(response.json()["code"], "files.not_found");
    }
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn files_011_the_comparison_records_a_picture_s_object_that_no_row_names() {
    let bucket = bucket::listing(&[
        ("pictures/settled", "2020-01-01T00:00:00.000Z"),
        ("pictures/named", "2020-01-01T00:00:00.000Z"),
    ])
    .await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
    let (_, workspace) = owned(&api).await;
    let db = api.database();
    db.owner_executes(&format!(
        "INSERT INTO workspace_pictures (workspace_id, media_type, size_bytes, crc32, object_key) \
         VALUES ('{workspace}', 'image/png', 5, 0, 'pictures/named')"
    ))
    .await;

    api.run_periodic("files.find_unreferenced_objects").await;

    assert_eq!(
        db.owner_reads("SELECT object_key FROM object_deletions")
            .await,
        ["pictures/settled"]
    );
}

#[tokio::test]
async fn files_011_a_replaced_picture_s_object_goes_once_its_delay_has_passed() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let db = api.database();
    link(&api, &ada, &workspace).await;
    let first = db
        .owner_reads("SELECT object_key FROM workspace_pictures")
        .await
        .remove(0);
    assert!(first.starts_with("pictures/"), "{first}");
    link(&api, &ada, &workspace).await;
    let second = db
        .owner_reads("SELECT object_key FROM workspace_pictures")
        .await
        .remove(0);
    // As if both objects were written more than an hour ago, and their delays had passed since.
    for key in [&first, &second] {
        db.owner_executes(&format!(
            "INSERT INTO object_deletions (object_key, deletable_at) \
             VALUES ('{key}', now() - interval '1 second')"
        ))
        .await;
    }

    api.run_periodic("files.delete_unreferenced_objects").await;

    assert_eq!(objects(&api).await, [second]);
}

#[tokio::test]
async fn files_013_a_picture_s_upload_is_counted_by_how_it_ended() {
    let api = Api::new().await;
    let (ada, workspace) = owned(&api).await;
    let (stored, unsupported) = (uploads("stored"), uploads("unsupported_type"));

    set(&api, &ada, &workspace, PNG).await;
    set(&api, &ada, &workspace, b"hello").await;

    // Tests running alongside upload too, so the counts grow by at least these.
    assert!(uploads("stored") >= stored + 1.0);
    assert!(uploads("unsupported_type") >= unsupported + 1.0);
}
