//! A user's own files (FILES-016 to FILES-019).

use serde_json::json;

use super::{objects, without_links};
use crate::support::{
    Api,
    api::Response,
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

#[tokio::test]
async fn files_003_an_upload_to_one_s_own_files_takes_only_an_accepted_kind_and_a_clean_scan() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let svg = upload_own(&api, &ada, "a.svg", b"<svg/>", "", None).await;
    let eicar = upload_own(&api, &ada, "a.txt", &scanner::eicar(), "", None).await;

    assert_eq!(svg.status, 422);
    assert_eq!(svg.json()["code"], "files.unsupported_type");
    assert_eq!(eicar.status, 422);
    assert_eq!(eicar.json()["code"], "files.malware_detected");
    assert!(own_names(&api, &ada.user_id).await.is_empty());
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
    let deleted = api
        .request(http::Method::DELETE, &format!("/v1/users/me/files/{id}"))
        .bearer(&grace.access)
        .send()
        .await;
    assert_eq!(deleted.status, 404);
    assert_eq!(deleted.json()["code"], "files.not_found");
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
