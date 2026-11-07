//! A workspace's files (the files spec).

use futures_util::TryStreamExt;
use platform::{
    storage::{Bytes, ObjectKey, Storage},
    telemetry::testing,
};
use serde_json::{Value, json};

use crate::support::{
    Api, TestDatabase,
    api::{Options, Response},
    bucket,
    events::{self, Event},
    files,
    lists::{field, pages},
    streams,
    users::{self, SignedIn},
    workspaces,
};

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

/// A workspace that `ada` owns and `grace` belongs to at `role`, with both signed in.
async fn acme(api: &Api, role: &str) -> (String, SignedIn, SignedIn) {
    let ada = users::register(api, "ada@example.com").await;
    let grace = users::register(api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &grace.user_id, role).await;
    (workspace, ada, grace)
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
        .map_ok(|key| key.as_str().to_owned())
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

/// Each audit entry of `action`, as its log, workspace, actor, resource and details.
async fn entries(api: &Api, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
             || resource_type || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = '{action}'"
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
    let mut bytes = Vec::new();
    let mut directory = Vec::new();
    for name in names {
        let offset = u32::try_from(bytes.len()).unwrap();
        let length = u16::try_from(name.len()).unwrap();
        bytes.extend_from_slice(b"PK\x03\x04");
        bytes.extend_from_slice(&[0; 22]);
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(name.as_bytes());
        directory.extend_from_slice(b"PK\x01\x02");
        directory.extend_from_slice(&[0; 24]);
        directory.extend_from_slice(&length.to_le_bytes());
        directory.extend_from_slice(&[0; 12]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }
    let offset = u32::try_from(bytes.len()).unwrap();
    let size = u32::try_from(directory.len()).unwrap();
    let count = u16::try_from(names.len()).unwrap();
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
            "workspace {workspace} {} file {id} {{}}",
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
    assert_eq!(retry.json(), first.json());
    assert_eq!(retry.header("location"), first.header("location"));
    assert_eq!(other.status, 422);
    assert_eq!(other.json()["code"], "request.idempotency_key_reused");
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
    assert_eq!(stored_names(&api, &workspace).await.len(), 2);
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
async fn files_006_an_upload_refuses_a_caller_before_reading_the_body() {
    let api = Api::new().await;
    let (workspace, _, _) = acme(&api, "member").await;
    let eve = users::register(&api, "eve@example.com").await;

    // Each of these bodies would be refused once read, and the name before it: the refusal of the
    // caller comes first.
    let empty = upload(&api, &eve, &workspace, "a.txt", b"", None).await;
    let unnamed = upload_with(&api, &eve, &workspace, &[], b"x", None).await;

    for refused in [&empty, &unnamed] {
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
    assert_eq!(read.json(), uploaded);
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
            "default-src 'none'; sandbox"
        );
        assert_eq!(response.header("cache-control"), "no-store");
    }
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
    let trace_id = response.json()["trace_id"].as_str().unwrap().to_owned();
    let errors: Vec<Value> = testing::logged(&trace_id)
        .into_iter()
        .filter(|line| line["level"] == "ERROR")
        .collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].to_string().contains(&id), "{errors:#?}");
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
            "workspace {workspace} {} file {id} {{}}",
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
async fn files_011_an_unreferenced_object_goes_once_its_delay_has_passed() {
    let api = Api::with(Options::default().set("FILE_DELETION_DELAY_DAYS", 3)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    let kept = files::arrange(db, &workspace, "Kept").await;
    let kept = object_key(&api, &kept).await;
    let deleted = files::arrange(db, &workspace, "Deleted").await;
    let orphan = object_key(&api, &deleted).await;
    delete(&api, &ada, &workspace, &deleted).await;

    api.run_periodic("files.find_unreferenced_objects").await;

    // Recorded 3 days from now, by the database's clock, and only the object no row names.
    assert_eq!(
        db.owner_reads(
            "SELECT object_key || ' ' || (deletable_at - now() BETWEEN interval '3 days' - \
             interval '1 minute' AND interval '3 days') FROM object_deletions"
        )
        .await,
        [format!("{orphan} true")]
    );
    // Before its time, nothing goes.
    api.run_periodic("files.delete_unreferenced_objects").await;
    let mut both = vec![kept.clone(), orphan.clone()];
    both.sort();
    assert_eq!(objects(&api).await, both);
    // A later run keeps the time it recorded.
    db.owner_executes("UPDATE object_deletions SET deletable_at = now() + interval '1 hour'")
        .await;
    api.run_periodic("files.find_unreferenced_objects").await;
    assert_eq!(
        db.owner_reads(
            "SELECT (deletable_at < now() + interval '2 hours')::text FROM object_deletions"
        )
        .await,
        ["true"]
    );

    db.owner_executes("UPDATE object_deletions SET deletable_at = now() - interval '1 second'")
        .await;
    api.run_periodic("files.delete_unreferenced_objects").await;

    assert_eq!(objects(&api).await, [kept]);
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

#[tokio::test]
async fn files_012_a_bucket_that_doesn_t_answer_makes_the_routes_of_bytes_answer_503() {
    // Nothing listens on port 9 of the loopback address.
    let api = Api::with(Options::default().set("S3_ENDPOINT", "http://127.0.0.1:9")).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;

    let uploaded = upload(&api, &ada, &workspace, "a.txt", b"hello", None).await;
    let downloaded = download(&api, &ada, &workspace, &id).await;

    for response in [&uploaded, &downloaded] {
        assert_eq!(response.status, 503);
        assert_eq!(response.json()["code"], "files.storage_unavailable");
        assert_eq!(response.header("retry-after"), "1");
    }
    assert_eq!(stored_names(&api, &workspace).await, ["Blue"]);
}

#[tokio::test]
async fn files_013_uploads_are_counted_by_how_they_ended() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let count = |outcome: &'static str| {
        testing::recorded("files.uploads")
            .into_iter()
            .filter(|point| point.labels.get("files.outcome").map(String::as_str) == Some(outcome))
            .map(|point| point.value)
            .sum::<f64>()
    };
    let (stored, unsupported) = (count("stored"), count("unsupported_type"));

    upload(&api, &ada, &workspace, "a.txt", b"hello", None).await;
    upload(&api, &ada, &workspace, "a.png", b"hello", None).await;

    // Tests running alongside upload too, so the counts grow by at least these.
    assert!(count("stored") >= stored + 1.0);
    assert!(count("unsupported_type") >= unsupported + 1.0);
}

#[tokio::test]
async fn sec_19_file_events_reach_the_members_and_no_replay_from_before_joining() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    let alan = users::register(&api, "alan@example.com").await;
    let db = api.database();
    // An event to Alan alone gives his stream one to resume after.
    db.owner_executes(&events::statement(
        &[Event::to_user(&alan.user_id, "note.made", "{}")],
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

/// `options`, with the settings of a copy of the test's bucket: the bucket `copy`, in the same
/// SeaweedFS (PLATFORM-077).
fn with_copy(options: Options, copy: &str) -> Options {
    options
        .set("S3_COPY_ENDPOINT", bucket::ENDPOINT)
        .set("S3_COPY_REGION", "us-east-1")
        .set("S3_COPY_BUCKET", copy)
        .set("S3_COPY_URI_STYLE", "path")
        .set("S3_COPY_ACCESS_KEY_ID", "test")
        .set("S3_COPY_SECRET_ACCESS_KEY", "test")
}

/// An API whose bucket has a copy, with the copy's name.
async fn api_with_copy(options: Options) -> (Api, String) {
    let database = TestDatabase::migrated().await;
    let copy = database.copy_bucket().await;
    let api = Api::on(database, with_copy(options, &copy)).await;
    (api, copy)
}

/// The client of the test's bucket and of its copy `copy`.
async fn both(api: &Api, copy: &str) -> Storage {
    bucket::storage_with_copy(&api.database().bucket().await, copy)
}

/// Writes the object `key` into the bucket `name`, with the bytes that builders arrange.
async fn put(name: &str, key: &str) {
    bucket::storage(name)
        .put(
            &ObjectKey::from_stored(key),
            Bytes::from_static(files::BYTES),
            "text/plain; charset=utf-8",
        )
        .await
        .unwrap();
}

/// Deletes the object `key` from the bucket `name`.
async fn remove_object(name: &str, key: &str) {
    bucket::storage(name)
        .delete(&ObjectKey::from_stored(key))
        .await
        .unwrap();
}

/// The keys of every object of the bucket `name`, in order.
async fn keys_of(name: &str) -> Vec<String> {
    bucket::storage(name)
        .list("")
        .map_ok(|key| key.as_str().to_owned())
        .try_collect()
        .await
        .unwrap()
}

/// Each `file.lost` entry, as its workspace, its file, whether it has no actor, and its details,
/// in the order of the files' IDs.
async fn lost_entries(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT workspace_id || ' ' || resource_id || ' ' || (actor_user_id IS NULL) || ' ' \
             || details::text FROM audit_entries WHERE action = 'file.lost' ORDER BY resource_id",
        )
        .await
}

#[tokio::test]
async fn files_011_the_deletion_of_bytes_finds_and_deletes_the_copy_s_objects_too() {
    let (api, copy) = api_with_copy(Options::default().set("FILE_DELETION_DELAY_DAYS", 3)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    let name = db.bucket().await;
    let deleted = files::arrange(db, &workspace, "Deleted").await;
    let in_both = object_key(&api, &deleted).await;
    put(&copy, &in_both).await;
    delete(&api, &ada, &workspace, &deleted).await;
    // An object that only the copy holds, and no row names, as after a restore of the bucket.
    let copy_alone = "workspaces/00000000000000000000000000000000";
    put(&copy, copy_alone).await;

    api.run_periodic("files.find_unreferenced_objects").await;
    let recorded = db
        .owner_reads("SELECT object_key FROM object_deletions ORDER BY object_key")
        .await;
    db.owner_executes("UPDATE object_deletions SET deletable_at = now() - interval '1 second'")
        .await;
    api.run_periodic("files.delete_unreferenced_objects").await;

    let mut expected = vec![copy_alone.to_owned(), in_both];
    expected.sort();
    assert_eq!(recorded, expected);
    assert!(keys_of(&name).await.is_empty());
    assert!(keys_of(&copy).await.is_empty());
}

#[tokio::test]
async fn files_014_a_download_while_the_bucket_restores_answers_503_until_its_bytes_are_back() {
    let (api, copy) = api_with_copy(Options::default()).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let id = files::arrange_row(api.database(), &workspace, "Blue").await;
    both(&api, &copy).await.mark_restoring().await.unwrap();

    let restoring = download(&api, &ada, &workspace, &id).await;
    put(&api.database().bucket().await, &object_key(&api, &id).await).await;
    let back = download(&api, &ada, &workspace, &id).await;

    assert_eq!(restoring.status, 503);
    assert_eq!(restoring.json()["code"], "files.restoring");
    assert_eq!(restoring.header("retry-after"), "60");
    assert_eq!(back.status, 200);
    assert_eq!(back.body, files::BYTES);
}

#[tokio::test]
async fn files_015_restore_files_removes_the_files_that_neither_bucket_holds_and_records_each() {
    let (api, copy) = api_with_copy(Options::default()).await;
    let (workspace, _, _) = acme(&api, "member").await;
    let db = api.database();
    let name = db.bucket().await;
    let other = workspaces::arrange(db, "Other", &[]).await;
    let in_both = files::arrange(db, &workspace, "Both").await;
    put(&copy, &object_key(&api, &in_both).await).await;
    let in_copy = files::arrange(db, &workspace, "Copy").await;
    let in_copy_key = object_key(&api, &in_copy).await;
    put(&copy, &in_copy_key).await;
    remove_object(&name, &in_copy_key).await;
    let lost = files::arrange_row(db, &workspace, "Lost").await;
    let lost_elsewhere = files::arrange_row(db, &other, "Lost too").await;

    let restored = app::restore_files(api.pool(), &both(&api, &copy).await, false)
        .await
        .unwrap();

    let mut expected = vec![(workspace.clone(), 1), (other.clone(), 1)];
    expected.sort();
    assert_eq!(
        restored,
        app::FilesRestored::Checked {
            lost: expected,
            restoring: true,
        }
    );
    assert_eq!(stored_names(&api, &workspace).await, ["Both", "Copy"]);
    assert!(stored_names(&api, &other).await.is_empty());
    let mut entries = vec![
        format!("{workspace} {lost} true {{}}"),
        format!("{other} {lost_elsewhere} true {{}}"),
    ];
    entries.sort_by_key(|entry| entry.split(' ').nth(1).unwrap().to_owned());
    assert_eq!(lost_entries(&api).await, entries);
    // A row that the server's loss took publishes no event, nor does its removal.
    assert!(published(&api, "file.deleted").await.is_empty());
    assert!(both(&api, &copy).await.restoring().await.unwrap());
}

#[tokio::test]
async fn files_015_a_bucket_that_holds_every_file_s_object_changes_nothing() {
    let (api, copy) = api_with_copy(Options::default()).await;
    let (workspace, _, _) = acme(&api, "member").await;
    let db = api.database();
    let kept = files::arrange(db, &workspace, "Kept").await;
    put(&copy, &object_key(&api, &kept).await).await;
    // Only in the bucket: an upload that the copy hasn't received yet.
    files::arrange(db, &workspace, "New").await;

    let restored = app::restore_files(api.pool(), &both(&api, &copy).await, false)
        .await
        .unwrap();

    assert_eq!(
        restored,
        app::FilesRestored::Checked {
            lost: Vec::new(),
            restoring: false,
        }
    );
    assert_eq!(stored_names(&api, &workspace).await, ["Kept", "New"]);
    assert!(!both(&api, &copy).await.restoring().await.unwrap());
}

#[tokio::test]
async fn files_015_with_when_empty_it_does_nothing_while_the_bucket_holds_an_object() {
    let (api, copy) = api_with_copy(Options::default()).await;
    let (workspace, _, _) = acme(&api, "member").await;
    let db = api.database();
    let name = db.bucket().await;
    let kept = files::arrange(db, &workspace, "Kept").await;
    let kept_key = object_key(&api, &kept).await;
    put(&copy, &kept_key).await;
    files::arrange_row(db, &workspace, "Lost").await;

    let holding = app::restore_files(api.pool(), &both(&api, &copy).await, true)
        .await
        .unwrap();
    let kept_rows = stored_names(&api, &workspace).await;
    // A new server's bucket, which holds nothing.
    remove_object(&name, &kept_key).await;
    let empty = app::restore_files(api.pool(), &both(&api, &copy).await, true)
        .await
        .unwrap();

    assert_eq!(holding, app::FilesRestored::NotEmpty);
    assert_eq!(kept_rows, ["Kept", "Lost"]);
    assert_eq!(
        empty,
        app::FilesRestored::Checked {
            lost: vec![(workspace.clone(), 1)],
            restoring: true,
        }
    );
    assert_eq!(stored_names(&api, &workspace).await, ["Kept"]);
}

#[tokio::test]
async fn files_015_a_copy_that_holds_nothing_removes_no_file() {
    let (api, copy) = api_with_copy(Options::default()).await;
    let (workspace, _, _) = acme(&api, "member").await;
    files::arrange_row(api.database(), &workspace, "Blue").await;

    let refused = app::restore_files(api.pool(), &both(&api, &copy).await, false).await;

    let error = platform::errors::chain(&refused.unwrap_err());
    assert!(error.contains("the copy holds no object"), "{error}");
    assert_eq!(stored_names(&api, &workspace).await, ["Blue"]);
}

#[tokio::test]
async fn files_015_the_subcommand_prints_how_many_files_each_workspace_lost() {
    let (api, copy) = api_with_copy(Options::default()).await;
    let (workspace, _, _) = acme(&api, "member").await;
    let db = api.database();
    let name = db.bucket().await;
    files::arrange_row(db, &workspace, "Lost").await;
    let kept = files::arrange(db, &workspace, "Kept").await;
    put(&copy, &object_key(&api, &kept).await).await;
    let bucket_settings = [
        ("S3_ENDPOINT", bucket::ENDPOINT),
        ("S3_REGION", "us-east-1"),
        ("S3_BUCKET", name.as_str()),
        ("S3_URI_STYLE", "path"),
        ("S3_ACCESS_KEY_ID", "test"),
        ("S3_SECRET_ACCESS_KEY", "test"),
    ];
    let copy_settings = [
        ("S3_COPY_ENDPOINT", bucket::ENDPOINT),
        ("S3_COPY_REGION", "us-east-1"),
        ("S3_COPY_BUCKET", copy.as_str()),
        ("S3_COPY_URI_STYLE", "path"),
        ("S3_COPY_ACCESS_KEY_ID", "test"),
        ("S3_COPY_SECRET_ACCESS_KEY", "test"),
    ];
    let settings: Vec<_> = bucket_settings
        .iter()
        .chain(&copy_settings)
        .copied()
        .collect();

    let without_copy = db.run_api(&["restore-files"], &bucket_settings).await;
    let ran = db.run_api(&["restore-files"], &settings).await;

    assert!(!without_copy.status.success());
    let refusal = String::from_utf8_lossy(&without_copy.stdout).into_owned()
        + &String::from_utf8_lossy(&without_copy.stderr);
    assert!(
        refusal.contains("S3_COPY_ENDPOINT is required"),
        "{refusal}"
    );
    assert!(ran.status.success(), "{ran:?}");
    let printed = String::from_utf8_lossy(&ran.stdout);
    assert!(printed.contains("Removed 1 files"), "{printed}");
    assert!(
        printed.contains(&format!("  workspace {workspace}: 1")),
        "{printed}"
    );
    assert!(
        printed.contains("holds every object of its copy"),
        "{printed}"
    );
    assert_eq!(stored_names(&api, &workspace).await, ["Kept"]);
}
