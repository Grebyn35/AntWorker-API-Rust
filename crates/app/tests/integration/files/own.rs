//! A user's own files (FILES-016 to FILES-019).

use serde_json::json;

use super::{objects, uploads, without_links};
use crate::support::{
    Api,
    api::{Options, Response},
    files,
    lists::{field, pages},
    scanner,
    users::{self, SignedIn},
};

/// Uploads `bytes` as the file `name` to `user`'s own files, with `query` as the request's query,
/// and `key` as its idempotency key if given.
pub(super) async fn upload_own(
    api: &Api,
    user: &SignedIn,
    name: &str,
    bytes: &[u8],
    query: &str,
    key: Option<&str>,
) -> Response {
    let mut call = api
        .request(http::Method::POST, &format!("/v1/users/me/files{query}"))
        .header("content-type", "application/octet-stream")
        .header(
            "content-disposition",
            &format!("attachment; filename=\"{name}\""),
        )
        .body(bytes.to_vec())
        .bearer(&user.access);
    if let Some(key) = key {
        call = call.header("idempotency-key", key);
    }
    call.send().await
}

/// Each audit entry of `action`, as its log, user, actor, resource and details.
async fn own_entries(api: &Api, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT scope || ' ' || user_id || ' ' || actor_user_id || ' ' || resource_type \
             || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = '{action}'"
        ))
        .await
}

/// Each event named `name`, as its scope and user, then its payload's file and how many fields it
/// holds.
async fn own_events(api: &Api, name: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT scope || ' ' || user_id || ' ' || (payload ->> 'file_id') || ' ' \
             || (SELECT count(*) FROM jsonb_object_keys(payload)) \
             FROM events WHERE name = '{name}'"
        ))
        .await
}

/// The names of `user`'s own files, in the order of their IDs.
async fn own_names(api: &Api, user: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT name FROM user_files WHERE user_id = '{user}' ORDER BY id"
        ))
        .await
}

#[tokio::test]
async fn files_016_uploading_to_one_s_own_files_stores_records_and_publishes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = upload_own(&api, &ada, "notes.md", b"# Notes\n", "", None).await;

    assert_eq!(response.status, 201);
    let file = response.json();
    let id = file["id"].as_str().unwrap();
    assert_eq!(
        response.header("location"),
        format!("/v1/users/me/files/{id}")
    );
    assert_eq!(
        without_links(&file),
        json!({
            "id": id,
            "user_id": ada.user_id,
            "name": "notes.md",
            "media_type": "text/markdown; charset=utf-8",
            "size_bytes": 8,
            "visibility": "private",
            "created_at": file["created_at"],
        })
    );
    assert_eq!(own_names(&api, &ada.user_id).await, ["notes.md"]);
    // The entry is in the caller's own log, and names no file name; the event reaches their
    // streams alone, with the file's ID.
    assert_eq!(
        own_entries(&api, "user_file.uploaded").await,
        [format!(
            "user {} {} user_file {id} {{}}",
            ada.user_id, ada.user_id
        )]
    );
    assert_eq!(
        own_events(&api, "user_file.uploaded").await,
        [format!("user {} {id} 1", ada.user_id)]
    );
}

#[tokio::test]
async fn files_016_a_retry_with_the_key_stores_one_file() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let first = upload_own(&api, &ada, "a.txt", b"hello", "", Some("\"k1\"")).await;
    let again = upload_own(&api, &ada, "a.txt", b"hello", "", Some("\"k1\"")).await;

    assert_eq!(first.status, 201);
    assert_eq!(again.status, 201);
    assert_eq!(first.json()["id"], again.json()["id"]);
    assert_eq!(own_names(&api, &ada.user_id).await, ["a.txt"]);
}

#[tokio::test]
async fn files_016_an_upload_without_a_credential_answers_401_with_the_body_unread() {
    let api = Api::new().await;

    // An empty body would be refused once read, and so would the missing name: the refusal of
    // the caller comes first.
    let response = api
        .request(http::Method::POST, "/v1/users/me/files")
        .header("content-type", "application/octet-stream")
        .send()
        .await;

    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "request.unauthenticated");
}

/// Sends `bytes` to `user`'s own files, with `headers` as the request's only headers but its
/// credential.
async fn send_own(api: &Api, user: &SignedIn, headers: &[(&str, &str)], bytes: &[u8]) -> Response {
    let mut call = api
        .request(http::Method::POST, "/v1/users/me/files")
        .body(bytes.to_vec())
        .bearer(&user.access);
    for (name, value) in headers {
        call = call.header(name, value);
    }
    call.send().await
}

#[tokio::test]
async fn files_002_an_upload_to_one_s_own_files_takes_its_name_from_content_disposition() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let octets = ("content-type", "application/octet-stream");

    let named = send_own(
        &api,
        &ada,
        &[
            octets,
            (
                "content-disposition",
                "attachment; filename*=UTF-8''caf%C3%A9.txt",
            ),
        ],
        b"x",
    )
    .await;
    let unnamed = send_own(&api, &ada, &[octets], b"x").await;
    let control = send_own(
        &api,
        &ada,
        &[
            octets,
            (
                "content-disposition",
                "attachment; filename*=UTF-8''a%07b.txt",
            ),
        ],
        b"x",
    )
    .await;

    assert_eq!(named.status, 201);
    assert_eq!(named.json()["name"], "caf\u{e9}.txt");
    for refused in [&unnamed, &control] {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "files.invalid_name");
    }
    assert_eq!(own_names(&api, &ada.user_id).await, ["caf\u{e9}.txt"]);
}

#[tokio::test]
async fn files_004_an_upload_to_one_s_own_files_over_the_limit_answers_413_and_an_empty_one_400() {
    let api = Api::with(Options::default().set("FILE_MAX_BYTES", 10)).await;
    let ada = users::register(&api, "ada@example.com").await;

    let fits = upload_own(&api, &ada, "a.txt", b"0123456789", "", None).await;
    let over = upload_own(&api, &ada, "b.txt", b"0123456789A", "", None).await;
    let empty = upload_own(&api, &ada, "c.txt", b"", "", None).await;

    assert_eq!(fits.status, 201);
    assert_eq!(over.status, 413);
    assert_eq!(over.json()["code"], "request.payload_too_large");
    assert_eq!(empty.status, 400);
    assert_eq!(empty.json()["code"], "files.empty");
    assert_eq!(own_names(&api, &ada.user_id).await, ["a.txt"]);
}

#[tokio::test]
async fn files_020_an_upload_to_one_s_own_files_of_another_visibility_answers_400_unread() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    // An empty body would be refused once read: the query's refusal comes first.
    let response = upload_own(&api, &ada, "a.txt", b"", "?visibility=everyone", None).await;

    assert_eq!(response.status, 400);
    assert_eq!(response.json()["code"], "request.invalid_query");
    assert_eq!(response.json()["errors"][0]["field"], "/visibility");
    assert!(own_names(&api, &ada.user_id).await.is_empty());
}

#[tokio::test]
async fn platform_074_a_user_s_upload_takes_only_application_octet_stream_and_a_valid_key() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let typed = send_own(
        &api,
        &ada,
        &[
            ("content-type", "text/plain"),
            ("content-disposition", "attachment; filename=\"a.txt\""),
        ],
        b"hello",
    )
    .await;
    let keyed = upload_own(&api, &ada, "a.txt", b"hello", "", Some("not a key")).await;

    assert_eq!(typed.status, 400);
    assert_eq!(typed.json()["code"], "request.invalid_body");
    assert_eq!(keyed.status, 400);
    assert_eq!(keyed.json()["code"], "request.invalid_idempotency_key");
    assert!(own_names(&api, &ada.user_id).await.is_empty());
}

#[tokio::test]
async fn files_003_an_upload_to_one_s_own_files_takes_only_an_accepted_kind() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let svg = upload_own(&api, &ada, "a.svg", b"<svg/>", "", None).await;

    assert_eq!(svg.status, 422);
    assert_eq!(svg.json()["code"], "files.unsupported_type");
    assert!(own_names(&api, &ada.user_id).await.is_empty());
}

#[tokio::test]
async fn files_005_an_upload_to_one_s_own_files_in_which_the_scanner_finds_a_signature_is_refused()
{
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let eicar = upload_own(&api, &ada, "a.txt", &scanner::eicar(), "", None).await;

    assert_eq!(eicar.status, 422);
    assert_eq!(eicar.json()["code"], "files.malware_detected");
    assert!(own_names(&api, &ada.user_id).await.is_empty());
    assert!(objects(&api).await.is_empty());
}

#[tokio::test]
async fn files_013_an_upload_to_one_s_own_files_counts_as_stored() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let stored = uploads("stored");

    let response = upload_own(&api, &ada, "a.txt", b"hello", "", None).await;

    assert_eq!(response.status, 201);
    // Tests running alongside upload too, so the count grows by at least this.
    assert!(uploads("stored") >= stored + 1.0);
}

#[tokio::test]
async fn files_012_a_bucket_that_doesn_t_answer_makes_one_s_own_upload_and_download_answer_503() {
    // Nothing listens on port 9 of the loopback address.
    let api = Api::with(Options::default().set("S3_ENDPOINT", "http://127.0.0.1:9")).await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = files::arrange_own(api.database(), &ada.user_id, "Blue").await;

    let uploaded = upload_own(&api, &ada, "a.txt", b"hello", "", None).await;
    let downloaded = api
        .get(&format!("/v1/users/me/files/{id}/content"))
        .bearer(&ada.access)
        .send()
        .await;

    for response in [&uploaded, &downloaded] {
        assert_eq!(response.status, 503);
        assert_eq!(response.json()["code"], "files.storage_unavailable");
        assert_eq!(response.header("retry-after"), "1");
    }
    assert_eq!(own_names(&api, &ada.user_id).await, ["Blue"]);
}

#[tokio::test]
async fn files_017_lists_one_s_own_files_the_latest_first_and_no_one_else_s() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    files::arrange_own(db, &ada.user_id, "First").await;
    files::arrange_own(db, &ada.user_id, "Second").await;
    files::arrange_own(db, &grace.user_id, "Grace's").await;

    let list = api
        .get("/v1/users/me/files")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(list.status, 200);
    let list = list.json();
    assert_eq!(field(&list, "name"), ["Second", "First"]);
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|file| file["user_id"] == ada.user_id)
    );
    let found = api
        .get("/v1/users/me/files?search=SEC")
        .bearer(&ada.access)
        .send()
        .await
        .json();
    assert_eq!(field(&found, "name"), ["Second"]);
}

#[tokio::test]
async fn files_017_paging_returns_each_file_once_across_a_shared_upload_time() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    for name in ["a", "b", "c", "d", "e"] {
        files::arrange_own(db, &ada.user_id, name).await;
    }
    db.owner_executes("UPDATE user_files SET created_at = '2026-01-01T00:00:00Z'")
        .await;

    let pages = pages(&api, &ada, "/v1/users/me/files?limit=2", "name").await;

    assert_eq!(pages.len(), 3);
    let mut names: Vec<String> = pages.into_iter().flatten().collect();
    names.sort();
    assert_eq!(names, ["a", "b", "c", "d", "e"]);
}

#[tokio::test]
async fn files_017_a_search_pages_through_its_matches() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    for name in ["Blue one", "Red", "Blue two", "Blue three"] {
        files::arrange_own(db, &ada.user_id, name).await;
    }

    let pages = pages(&api, &ada, "/v1/users/me/files?search=BLUE&limit=2", "name").await;

    assert_eq!(pages, [vec!["Blue three", "Blue two"], vec!["Blue one"]]);
}

#[tokio::test]
async fn files_018_reading_one_s_own_file_gives_it_and_another_s_answers_404() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let uploaded = upload_own(&api, &ada, "a.txt", b"hello", "", None)
        .await
        .json();
    let id = uploaded["id"].as_str().unwrap();

    let read = api
        .get(&format!("/v1/users/me/files/{id}"))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(read.status, 200);
    assert_eq!(without_links(&read.json()), without_links(&uploaded));

    // Another user's file, and an ID that isn't a UUID, name no file of the caller's, here and
    // in its download and its deletion.
    for path in [
        format!("/v1/users/me/files/{id}"),
        format!("/v1/users/me/files/{id}/content"),
        "/v1/users/me/files/not-a-uuid".to_owned(),
        "/v1/users/me/files/not-a-uuid/content".to_owned(),
    ] {
        let response = api.get(&path).bearer(&grace.access).send().await;
        assert_eq!(response.status, 404, "{path}");
        assert_eq!(response.json()["code"], "files.not_found", "{path}");
    }
    for path in [
        format!("/v1/users/me/files/{id}"),
        "/v1/users/me/files/not-a-uuid".to_owned(),
    ] {
        let deleted = api
            .request(http::Method::DELETE, &path)
            .bearer(&grace.access)
            .send()
            .await;
        assert_eq!(deleted.status, 404, "{path}");
        assert_eq!(deleted.json()["code"], "files.not_found", "{path}");
    }
    assert_eq!(own_names(&api, &ada.user_id).await, ["a.txt"]);
}

#[tokio::test]
async fn files_009_downloading_one_s_own_file_serves_it_so_that_it_can_t_run() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = upload_own(&api, &ada, "notes.txt", b"hello", "", None)
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = api
        .get(&format!("/v1/users/me/files/{id}/content"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"hello");
    assert_eq!(response.header("content-type"), "text/plain; charset=utf-8");
    assert_eq!(
        response.header("content-disposition"),
        "attachment; filename=\"notes.txt\"; filename*=UTF-8''notes.txt"
    );
    assert_eq!(
        response.header("content-security-policy"),
        "default-src 'none'; sandbox"
    );
    assert_eq!(response.header("x-content-type-options"), "nosniff");
    assert_eq!(response.header("cache-control"), "no-store");
}

#[tokio::test]
async fn files_019_deleting_one_s_own_file_deletes_its_row_records_and_publishes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = upload_own(&api, &ada, "a.txt", b"hello", "", None)
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stored = objects(&api).await;

    let response = api
        .request(http::Method::DELETE, &format!("/v1/users/me/files/{id}"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 204);
    assert!(own_names(&api, &ada.user_id).await.is_empty());
    assert_eq!(
        own_entries(&api, "user_file.deleted").await,
        [format!(
            "user {} {} user_file {id} {{}}",
            ada.user_id, ada.user_id
        )]
    );
    assert_eq!(
        own_events(&api, "user_file.deleted").await,
        [format!("user {} {id} 1", ada.user_id)]
    );
    // Its bytes stay until the deletion of unreferenced objects (FILES-011).
    assert_eq!(objects(&api).await, stored);
    let gone = api
        .get(&format!("/v1/users/me/files/{id}"))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(gone.status, 404);
}

#[tokio::test]
async fn files_011_a_deleted_own_file_s_object_goes_once_its_delay_has_passed() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let id = upload_own(&api, &ada, "a.txt", b"hello", "", None)
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let key = db
        .owner_reads(&format!(
            "SELECT object_key FROM user_files WHERE id = '{id}'"
        ))
        .await
        .remove(0);
    assert!(key.starts_with("users/"), "{key}");
    api.request(http::Method::DELETE, &format!("/v1/users/me/files/{id}"))
        .bearer(&ada.access)
        .send()
        .await;
    // As if the object were written more than an hour ago, and its delay had passed since.
    db.owner_executes(&format!(
        "INSERT INTO object_deletions (object_key, deletable_at) \
         VALUES ('{key}', now() - interval '1 second')"
    ))
    .await;

    api.run_periodic("files.delete_unreferenced_objects").await;

    assert!(objects(&api).await.is_empty());
}

#[tokio::test]
async fn files_011_a_recorded_object_that_a_user_s_row_names_stays() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let id = files::arrange_own(db, &ada.user_id, "Kept").await;
    let key = db
        .owner_reads(&format!(
            "SELECT object_key FROM user_files WHERE id = '{id}'"
        ))
        .await
        .remove(0);
    // A comparison recorded the object while no row named it, as before a restore brought the
    // row back, and its time has passed.
    db.owner_executes(&format!(
        "INSERT INTO object_deletions (object_key, deletable_at) \
         VALUES ('{key}', now() - interval '1 second')"
    ))
    .await;

    api.run_periodic("files.delete_unreferenced_objects").await;

    assert_eq!(objects(&api).await, [key]);
    assert!(
        db.owner_reads("SELECT object_key FROM object_deletions")
            .await
            .is_empty()
    );
}
