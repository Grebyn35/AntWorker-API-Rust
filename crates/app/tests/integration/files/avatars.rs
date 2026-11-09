//! Users' avatars: setting and removing one through `accounts`, which `files` keeps, its links,
//! and its public download (ACCOUNTS-008 to ACCOUNTS-011, WORKSPACES-010, FILES-011, FILES-013,
//! FILES-024).

use std::time::Duration;

use axum::body::Body;
use bytes::Bytes;
use futures_util::stream;
use serde_json::json;

use super::{errors_of, objects, uploads};
use crate::support::{
    Api,
    api::{Options, Response},
    bucket,
    scanner::{self, Script},
    users::{self, SignedIn},
    workspaces,
};

/// The smallest PNG's bytes: its signature, then its header chunk.
const PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";

/// Sets `bytes` as `user`'s avatar.
async fn set(api: &Api, user: &SignedIn, bytes: &[u8]) -> Response {
    api.request(http::Method::PUT, "/v1/users/me/avatar")
        .header("content-type", "application/octet-stream")
        .body(bytes.to_vec())
        .bearer(&user.access)
        .send()
        .await
}

/// Removes `user`'s avatar.
async fn remove(api: &Api, user: &SignedIn) -> Response {
    api.request(http::Method::DELETE, "/v1/users/me/avatar")
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

/// The record's `ETag` that `user` reads.
async fn etag(api: &Api, user: &SignedIn) -> String {
    api.get("/v1/users/me")
        .bearer(&user.access)
        .send()
        .await
        .header("etag")
        .to_owned()
}

/// Downloads the avatar whose link is `url`, without a credential.
async fn download(api: &Api, url: &str) -> Response {
    api.get(url).send().await
}

/// The avatars' rows, each as its user, media type and size.
async fn rows(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT user_id || ' ' || media_type || ' ' || size_bytes FROM avatars ORDER BY id",
        )
        .await
}

/// The ID that an avatar's link names.
fn id_of(url: &str) -> &str {
    url.strip_prefix("/v1/public/avatars/").unwrap()
}

#[tokio::test]
async fn accounts_008_setting_an_avatar_stores_it_records_it_and_publishes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(db, "Acme", &[(&grace.user_id, "owner")]).await;
    workspaces::join(db, &acme, &ada.user_id, "member").await;

    let response = set(&api, &ada, PNG).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("etag"), "\"2\"");
    let body = response.json();
    assert_eq!(body["id"], ada.user_id);
    assert_eq!(body["name"], "Ada Lovelace");
    let url = body["avatar_url"].as_str().unwrap().to_owned();
    let id = id_of(&url);
    assert_eq!(
        rows(&api).await,
        [format!("{} image/png {}", ada.user_id, PNG.len())]
    );
    let downloaded = download(&api, &url).await;
    assert_eq!(downloaded.status, 200);
    assert_eq!(downloaded.header("content-type"), "image/png");
    assert_eq!(downloaded.body, PNG);
    let ada_id = &ada.user_id;
    let entries = db
        .owner_reads(
            "SELECT scope || ' ' || user_id || ' ' || actor_user_id || ' ' || resource_type \
             || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = 'avatar.uploaded'",
        )
        .await;
    assert_eq!(
        entries,
        [format!("user {ada_id} {ada_id} avatar {id} {{}}")]
    );
    let events = db
        .owner_reads(
            "SELECT name || ' ' || coalesce(workspace_id::text, '-') || ' ' \
             || coalesce(permission, '-') FROM events ORDER BY name",
        )
        .await;
    assert_eq!(
        events,
        [
            format!("member.updated {acme} members.read"),
            "user.updated - -".to_owned()
        ]
    );
}

#[tokio::test]
async fn accounts_008_a_new_avatar_replaces_the_one_before_whose_bytes_stay() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let first = set(&api, &ada, PNG).await.json()["avatar_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let gif = b"GIF89a\x01\0\x01\0\0\0\0;";

    let second = set(&api, &ada, gif).await;

    assert_eq!(second.status, 200);
    assert_eq!(second.header("etag"), "\"3\"");
    let url = second.json()["avatar_url"].as_str().unwrap().to_owned();
    assert_ne!(url, first);
    assert_eq!(
        rows(&api).await,
        [format!("{} image/gif {}", ada.user_id, gif.len())]
    );
    assert_eq!(download(&api, &first).await.status, 404);
    assert_eq!(download(&api, &url).await.status, 200);
    // The replaced image's object stays until the deletion of unreferenced objects (FILES-011).
    assert_eq!(objects(&api).await.len(), 2);
}

#[tokio::test]
async fn accounts_008_setting_an_avatar_without_a_credential_answers_401_with_its_body_unread() {
    let api = Api::new().await;

    let response = api
        .request(http::Method::PUT, "/v1/users/me/avatar")
        .header("content-type", "application/octet-stream")
        .send_streamed(unread())
        .await;

    assert_eq!(response.status, 401);
    assert!(rows(&api).await.is_empty());
    assert!(objects(&api).await.is_empty());
}

#[tokio::test]
async fn accounts_009_an_avatar_is_a_png_jpeg_gif_or_webp_image_its_kind_from_its_bytes() {
    let api = Api::new().await;
    let images: [(&[u8], &str); 4] = [
        (PNG, "image/png"),
        (b"\xff\xd8\xff\xe0\0\x10JFIF", "image/jpeg"),
        (b"GIF87a\x01\0\x01\0\0\0\0;", "image/gif"),
        (b"RIFF\x1a\0\0\0WEBPVP8 ", "image/webp"),
    ];

    for (index, (bytes, media_type)) in images.into_iter().enumerate() {
        let user = users::register(&api, &format!("user{index}@example.com")).await;
        let response = set(&api, &user, bytes).await;

        assert_eq!(response.status, 200, "{media_type}");
        let url = response.json()["avatar_url"].as_str().unwrap().to_owned();
        let downloaded = download(&api, &url).await;
        assert_eq!(downloaded.header("content-type"), media_type);
    }
}

#[tokio::test]
async fn accounts_009_an_empty_body_or_one_that_is_no_such_image_answers_422() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let refused: [&[u8]; 4] = [b"", b"hello", b"%PDF-1.7", b"<svg></svg>"];

    for bytes in refused {
        let response = set(&api, &ada, bytes).await;

        assert_eq!(response.status, 422, "{bytes:?}");
        assert_eq!(response.json()["code"], "accounts.avatar_not_image");
    }
    assert!(rows(&api).await.is_empty());
    assert!(objects(&api).await.is_empty());
}

#[tokio::test]
async fn accounts_009_a_body_of_more_than_1_mib_answers_413() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut large = PNG.to_vec();
    large.resize(1_048_577, 0);
    let mut largest = PNG.to_vec();
    largest.resize(1_048_576, 0);

    let refused = set(&api, &ada, &large).await;
    let accepted = set(&api, &ada, &largest).await;

    assert_eq!(refused.status, 413);
    assert_eq!(refused.json()["code"], "request.payload_too_large");
    assert_eq!(accepted.status, 200);
    assert_eq!(
        rows(&api).await,
        [format!("{} image/png 1048576", ada.user_id)]
    );
}

#[tokio::test]
async fn accounts_009_an_avatar_in_which_the_scanner_finds_malware_answers_422() {
    let stand_in = scanner::scripted(Script::Answer("stream: Eicar-Test-Signature FOUND")).await;
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", &stand_in.address)).await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = set(&api, &ada, PNG).await;

    assert_eq!(response.status, 422);
    assert_eq!(response.json()["code"], "accounts.avatar_malware_detected");
    assert!(rows(&api).await.is_empty());
    assert!(objects(&api).await.is_empty());
}

#[tokio::test]
async fn accounts_009_while_the_scanner_or_the_storage_doesn_t_answer_an_avatar_answers_503() {
    // Nothing listens on port 9 of the loopback address.
    let scanner_down = Api::with(Options::default().set("CLAMD_ADDRESS", "127.0.0.1:9")).await;
    let storage_down = Api::with(Options::default().set("S3_ENDPOINT", "http://127.0.0.1:9")).await;

    for api in [&scanner_down, &storage_down] {
        let ada = users::register(api, "ada@example.com").await;
        let response = set(api, &ada, PNG).await;

        assert_eq!(response.status, 503);
        assert_eq!(response.json()["code"], "accounts.avatar_unavailable");
        assert_eq!(response.header("retry-after"), "1");
        assert!(rows(api).await.is_empty());
        let errors = errors_of(&response);
        assert_eq!(errors.len(), 1, "{errors:#?}");
    }
}

#[tokio::test]
async fn accounts_010_removing_an_avatar_deletes_it_records_it_and_publishes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let acme = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    let url = set(&api, &ada, PNG).await.json()["avatar_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = id_of(&url).to_owned();
    let db = api.database();
    db.owner_executes("DELETE FROM events").await;

    let response = remove(&api, &ada).await;

    assert_eq!(response.status, 204);
    assert!(rows(&api).await.is_empty());
    assert_eq!(download(&api, &url).await.status, 404);
    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(me.json()["avatar_url"], json!(null));
    assert_eq!(me.header("etag"), "\"3\"");
    let ada_id = &ada.user_id;
    let entries = db
        .owner_reads(
            "SELECT scope || ' ' || user_id || ' ' || resource_type || ' ' || resource_id \
             FROM audit_entries WHERE action = 'avatar.deleted'",
        )
        .await;
    assert_eq!(entries, [format!("user {ada_id} avatar {id}")]);
    assert_eq!(
        db.owner_reads(
            "SELECT name || ' ' || coalesce(workspace_id::text, '-') || ' ' \
             || coalesce(permission, '-') FROM events ORDER BY name"
        )
        .await,
        [
            format!("member.updated {acme} members.read"),
            "user.updated - -".to_owned()
        ]
    );
    // Its object stays until the deletion of unreferenced objects (FILES-011).
    assert_eq!(objects(&api).await.len(), 1);
}

#[tokio::test]
async fn accounts_010_removing_no_avatar_answers_204_and_changes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = remove(&api, &ada).await;

    assert_eq!(response.status, 204);
    let recorded = api
        .database()
        .owner_reads(&format!(
            "SELECT (SELECT version FROM users WHERE id = '{}') || ' ' \
             || (SELECT count(*) FROM audit_entries WHERE action = 'avatar.deleted') || ' ' \
             || (SELECT count(*) FROM events)",
            ada.user_id
        ))
        .await;
    assert_eq!(recorded, ["1 0 0"]);
}

#[tokio::test]
async fn accounts_011_the_record_and_each_profile_carry_the_avatar_s_link() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &acme, &grace.user_id, "member").await;
    let before = api.get("/v1/users/me").bearer(&ada.access).send().await;

    let url = set(&api, &ada, PNG).await.json()["avatar_url"]
        .as_str()
        .unwrap()
        .to_owned();

    assert_eq!(before.json()["avatar_url"], json!(null));
    let me = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(me.json()["avatar_url"], url);
    let members = api
        .get(&format!("/v1/workspaces/{acme}/members"))
        .bearer(&grace.access)
        .send()
        .await;
    let links: Vec<_> = members.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| (member["user_id"].clone(), member["avatar_url"].clone()))
        .collect();
    assert_eq!(
        links,
        [
            (json!(grace.user_id), json!(null)),
            (json!(ada.user_id), json!(url))
        ]
    );
}

#[tokio::test]
async fn workspaces_010_each_member_carries_the_link_of_their_avatar() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &acme, &grace.user_id, "member").await;
    let url = set(&api, &grace, PNG).await.json()["avatar_url"].clone();

    let members = api
        .get(&format!("/v1/workspaces/{acme}/members"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(members.json()["items"][0]["user_id"], grace.user_id);
    assert_eq!(members.json()["items"][0]["avatar_url"], url);
    assert_eq!(members.json()["items"][1]["avatar_url"], json!(null));
}

#[tokio::test]
async fn files_024_an_avatar_s_download_needs_no_credential_and_names_it_by_its_id() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let url = set(&api, &ada, PNG).await.json()["avatar_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = id_of(&url);

    let response = download(&api, &url).await;

    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-disposition"),
        format!("inline; filename=\"{id}.png\"; filename*=UTF-8''{id}.png")
    );
    assert_eq!(response.header("cache-control"), "public, max-age=300");
    assert_eq!(response.header("etag"), format!("\"{id}\""));
    let unknown = download(
        &api,
        "/v1/public/avatars/0198f1a2-0000-7000-8000-000000000000",
    )
    .await;
    let not_uuid = download(&api, "/v1/public/avatars/not-a-uuid").await;
    for missing in [&unknown, &not_uuid] {
        assert_eq!(missing.status, 404);
        assert_eq!(missing.json()["code"], "files.not_found");
    }
}

#[tokio::test]
async fn files_011_the_comparison_records_an_avatar_s_object_that_no_row_names() {
    let bucket = bucket::listing(&[
        ("avatars/settled", "2020-01-01T00:00:00.000Z"),
        ("avatars/named", "2020-01-01T00:00:00.000Z"),
    ])
    .await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    db.owner_executes(&format!(
        "INSERT INTO avatars (user_id, media_type, size_bytes, object_key) \
         VALUES ('{}', 'image/png', 5, 'avatars/named')",
        ada.user_id
    ))
    .await;

    api.run_periodic("files.find_unreferenced_objects").await;

    assert_eq!(
        db.owner_reads("SELECT object_key FROM object_deletions")
            .await,
        ["avatars/settled"]
    );
}

#[tokio::test]
async fn files_011_a_replaced_avatar_s_object_goes_once_its_delay_has_passed() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    set(&api, &ada, PNG).await;
    let first = db
        .owner_reads("SELECT object_key FROM avatars")
        .await
        .remove(0);
    assert!(first.starts_with("avatars/"), "{first}");
    set(&api, &ada, PNG).await;
    let second = db
        .owner_reads("SELECT object_key FROM avatars")
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
async fn files_013_an_avatar_s_upload_is_counted_by_how_it_ended() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let (stored, unsupported) = (uploads("stored"), uploads("unsupported_type"));

    set(&api, &ada, PNG).await;
    set(&api, &ada, b"hello").await;

    // Tests running alongside upload too, so the counts grow by at least these.
    assert!(uploads("stored") >= stored + 1.0);
    assert!(uploads("unsupported_type") >= unsupported + 1.0);
}

#[tokio::test]
async fn accounts_009_a_declared_length_over_1_mib_answers_413_before_the_body_is_read() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = api
        .request(http::Method::PUT, "/v1/users/me/avatar")
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
async fn accounts_009_a_scanner_that_doesn_t_answer_within_its_time_answers_503() {
    let stand_in = scanner::scripted(Script::Never).await;
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", &stand_in.address)).await;
    let ada = users::register(&api, "ada@example.com").await;
    let clock = api.clock();
    // A scan has 30 seconds (the platform spec's Scanner).
    let wait = Duration::from_secs(30);

    let (response, ()) = tokio::join!(set(&api, &ada, PNG), async {
        stand_in.read.notified().await;
        clock.sleeping(wait).await;
        clock.advance(wait);
    });

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "accounts.avatar_unavailable");
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn accounts_009_an_avatar_that_its_route_s_timeout_ends_answers_503_timed_out() {
    let stand_in = scanner::scripted(Script::Never).await;
    let api = Api::with(Options::default().set("CLAMD_ADDRESS", &stand_in.address)).await;
    let ada = users::register(&api, "ada@example.com").await;

    let (response, ()) = tokio::join!(set(&api, &ada, PNG), async {
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
async fn accounts_008_setting_an_avatar_with_a_stale_etag_answers_412() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let stale = etag(&api, &ada).await;
    set(&api, &ada, PNG).await;

    let response = api
        .request(http::Method::PUT, "/v1/users/me/avatar")
        .header("content-type", "application/octet-stream")
        .header("if-match", &stale)
        .body(PNG.to_vec())
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 412);
    assert_eq!(response.json()["code"], "request.precondition_failed");
    assert_eq!(rows(&api).await.len(), 1);
    let recorded = api
        .database()
        .owner_reads("SELECT count(*)::text FROM audit_entries WHERE action = 'avatar.uploaded'")
        .await;
    assert_eq!(recorded, ["1"]);
}

#[tokio::test]
async fn accounts_010_removing_an_avatar_with_a_stale_etag_answers_412_and_with_the_current_204() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let stale = etag(&api, &ada).await;
    set(&api, &ada, PNG).await;
    let current = etag(&api, &ada).await;
    let remove_with = |etag: &str| {
        api.request(http::Method::DELETE, "/v1/users/me/avatar")
            .header("if-match", etag)
            .bearer(&ada.access)
    };

    let refused = remove_with(&stale).send().await;
    let removed = remove_with(&current).send().await;

    assert_eq!(refused.status, 412);
    assert_eq!(refused.json()["code"], "request.precondition_failed");
    assert_eq!(removed.status, 204);
    assert!(rows(&api).await.is_empty());
}

#[tokio::test]
async fn accounts_008_setting_an_avatar_reads_no_idempotency_key() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    // A key that isn't one would answer 400 on a route that read it.
    let response = api
        .request(http::Method::PUT, "/v1/users/me/avatar")
        .header("content-type", "application/octet-stream")
        .header("idempotency-key", "not a key")
        .body(PNG.to_vec())
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 200);
}

#[tokio::test]
async fn files_024_an_avatar_s_download_serves_a_range_of_its_bytes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let url = set(&api, &ada, PNG).await.json()["avatar_url"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = api.get(&url).header("range", "bytes=0-7").send().await;

    assert_eq!(response.status, 206);
    assert_eq!(
        response.header("content-range"),
        format!("bytes 0-7/{}", PNG.len())
    );
    assert_eq!(response.body, &PNG[..8]);
}

#[tokio::test]
async fn files_024_an_avatar_s_download_counts_in_the_bucket_of_public_downloads() {
    let api = Api::with(Options::default().set("RATE_LIMIT_PUBLIC_REQUESTS", 7)).await;
    let ada = users::register(&api, "ada@example.com").await;
    let url = set(&api, &ada, PNG).await.json()["avatar_url"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = download(&api, &url).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("ratelimit-policy"), "\"public\";q=7;w=60");
}

#[tokio::test]
async fn files_024_a_replaced_or_removed_avatar_s_download_answers_404() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let replaced = set(&api, &ada, PNG).await.json()["avatar_url"]
        .as_str()
        .unwrap()
        .to_owned();
    let removed = set(&api, &ada, PNG).await.json()["avatar_url"]
        .as_str()
        .unwrap()
        .to_owned();
    remove(&api, &ada).await;

    for url in [&replaced, &removed] {
        let response = download(&api, url).await;

        assert_eq!(response.status, 404);
        assert_eq!(response.json()["code"], "files.not_found");
    }
}

#[tokio::test]
async fn accounts_009_while_the_storage_doesn_t_answer_within_its_time_an_avatar_answers_503() {
    let bucket = bucket::scripted(None).await;
    let api = Api::with(Options::default().set("S3_ENDPOINT", &bucket.endpoint)).await;
    let ada = users::register(&api, "ada@example.com").await;
    let clock = api.clock();
    // An upload waits 15 seconds for the bucket's answer, within its route's 60 (the platform
    // spec's Storage).
    let wait = Duration::from_secs(15);

    let (response, ()) = tokio::join!(set(&api, &ada, PNG), async {
        clock.sleeping(wait).await;
        clock.advance(wait);
    });

    assert_eq!(response.status, 503);
    assert_eq!(response.json()["code"], "accounts.avatar_unavailable");
    assert!(rows(&api).await.is_empty());
}
