//! What an export holds of files, and the checksum each upload records (FILES-025, FILES-026).

use platform::storage::ObjectKey;
use serde_json::json;

use super::{own::upload_own, upload};
use crate::{
    accounts::exports::{built, document, entries},
    support::{Api, api::Response, bucket, files, users, workspaces},
};

/// Sets `user`'s avatar to `bytes`.
async fn set_avatar(api: &Api, access: &str, bytes: &[u8]) -> Response {
    api.request(http::Method::PUT, "/v1/users/me/avatar")
        .header("content-type", "application/octet-stream")
        .body(bytes.to_vec())
        .bearer(access)
        .send()
        .await
}

/// Sets the picture of `workspace` to `bytes`.
async fn set_picture(api: &Api, access: &str, workspace: &str, bytes: &[u8]) -> Response {
    api.request(
        http::Method::PUT,
        &format!("/v1/workspaces/{workspace}/picture"),
    )
    .header("content-type", "application/octet-stream")
    .body(bytes.to_vec())
    .bearer(access)
    .send()
    .await
}

/// Downloads the archive of the export `id` with `access`.
async fn archive(api: &Api, access: &str, id: &str) -> Vec<u8> {
    let response = api
        .get(&format!("/v1/users/me/exports/{id}/content"))
        .bearer(access)
        .send()
        .await;
    assert_eq!(response.status, 200);
    response.body
}

#[tokio::test]
async fn files_025_every_upload_records_the_crc_32_of_its_bytes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let acme = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;

    let own = upload_own(&api, &ada, "notes.txt", b"hello there", "", None).await;
    let shared = upload(&api, &ada, &acme, "plan.txt", b"the plan", None).await;
    let avatar = set_avatar(&api, &ada.access, files::PNG).await;
    let gif = b"GIF89a\x01\0\x01\0\0\0\0;";
    let picture = set_picture(&api, &ada.access, &acme, gif).await;

    assert_eq!(own.status, 201);
    assert_eq!(shared.status, 201);
    assert_eq!(avatar.status, 200);
    assert_eq!(picture.status, 200);
    let db = api.database();
    let recorded = [
        db.owner_reads("SELECT crc32::text FROM user_files").await,
        db.owner_reads("SELECT crc32::text FROM workspace_files")
            .await,
        db.owner_reads("SELECT crc32::text FROM avatars").await,
        db.owner_reads("SELECT crc32::text FROM workspace_pictures")
            .await,
    ];
    let expected = [b"hello there".as_slice(), b"the plan", files::PNG, gif]
        .map(|bytes| vec![platform::archive::crc32(bytes).to_string()]);
    assert_eq!(recorded, expected);
}

#[tokio::test]
async fn files_026_an_export_holds_the_user_s_files_their_avatar_and_their_own_workspaces_files_and_pictures()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let own_workspace = workspaces::arrange(db, "Mine", &[(&ada.user_id, "owner")]).await;
    let shared = workspaces::arrange(
        db,
        "Theirs",
        &[(&grace.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    let own = upload_own(&api, &ada, "a/b: c.txt", b"my own words", "", None)
        .await
        .json();
    let avatar = set_avatar(&api, &ada.access, files::PNG).await;
    assert_eq!(avatar.status, 200);
    let mine = files::arrange(db, &own_workspace, "notes.md").await;
    // A file Ada uploaded to a workspace that has another member is the workspace's, and so is
    // that workspace's picture.
    files::arrange_uploaded(db, &shared, "shared.txt", &ada.user_id).await;
    let gif = b"GIF89a\x01\0\x01\0\0\0\0;";
    let picture = set_picture(&api, &ada.access, &own_workspace, gif).await;
    assert_eq!(picture.status, 200);
    let theirs = set_picture(&api, &grace.access, &shared, files::PNG).await;
    assert_eq!(theirs.status, 200);

    let id = built(&api, &ada).await;
    let archive = archive(&api, &ada.access, &id).await;

    let own_id = own["id"].as_str().unwrap();
    let records = document(&archive, "files/files.json");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "a/b: c.txt");
    assert_eq!(records[0]["path"], format!("files/own/{own_id}/a_b_ c.txt"));
    let avatars = document(&archive, "files/avatar.json");
    let avatar_id = avatars[0]["id"].as_str().unwrap();
    assert_eq!(avatars[0]["path"], format!("files/avatar/{avatar_id}.png"));
    let held = document(&archive, "files/workspace_files.json");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["id"], mine.as_str());
    assert_eq!(held[0]["workspace_id"], own_workspace.as_str());
    assert_eq!(held[0]["uploader_id"], json!(null));
    let pictures = document(&archive, "files/workspace_pictures.json");
    assert_eq!(pictures.len(), 1);
    let picture_id = pictures[0]["id"].as_str().unwrap();
    assert_eq!(pictures[0]["workspace_id"], own_workspace.as_str());
    assert_eq!(pictures[0]["media_type"], "image/gif");
    assert_eq!(pictures[0]["size_bytes"], gif.len());
    let bytes: Vec<(String, Vec<u8>)> = entries(&archive)
        .into_iter()
        .filter(|(path, _)| !path.ends_with(".json"))
        .collect();
    assert_eq!(
        bytes,
        [
            (
                format!("files/own/{own_id}/a_b_ c.txt"),
                b"my own words".to_vec()
            ),
            (format!("files/avatar/{avatar_id}.png"), files::PNG.to_vec()),
            (
                format!("files/workspaces/{own_workspace}/{mine}/notes.md"),
                files::BYTES.to_vec()
            ),
            (
                format!("files/workspace_pictures/{own_workspace}/{picture_id}.gif"),
                gif.to_vec()
            ),
        ]
    );
}

#[tokio::test]
async fn files_026_a_held_file_s_bytes_stay_while_its_export_lasts_though_the_file_is_deleted() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let own = upload_own(&api, &ada, "notes.txt", b"kept for the export", "", None)
        .await
        .json();
    let file = own["id"].as_str().unwrap().to_owned();
    let id = built(&api, &ada).await;
    let db = api.database();
    let key = db
        .owner_reads("SELECT object_key FROM exported_files")
        .await
        .remove(0);
    let deleted = api
        .request(http::Method::DELETE, &format!("/v1/users/me/files/{file}"))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(deleted.status, 204);
    // The object is due for deletion, as the comparison would record it once no file names it.
    db.owner_executes(&format!(
        "INSERT INTO object_deletions (object_key, deletable_at) \
         VALUES ('{key}', now() - interval '1 second')"
    ))
    .await;
    let storage = bucket::storage(&db.bucket().await);
    let object = ObjectKey::from_stored(&key);

    api.run_periodic("files.delete_unreferenced_objects").await;

    let kept = storage.get(&object, None).await;
    assert!(kept.is_ok(), "the held object was deleted");
    let archive = archive(&api, &ada.access, &id).await;
    let (_, bytes) = entries(&archive)
        .into_iter()
        .find(|(path, _)| path.starts_with("files/own/"))
        .unwrap();
    assert_eq!(bytes, b"kept for the export");

    // Once the export expires, nothing names the object, which goes.
    db.owner_executes("UPDATE exports SET expires_at = now() - interval '1 second'")
        .await;
    api.run_periodic("accounts.expire_exports").await;
    db.owner_executes(&format!(
        "INSERT INTO object_deletions (object_key, deletable_at) \
         VALUES ('{key}', now() - interval '1 second') ON CONFLICT DO NOTHING"
    ))
    .await;
    api.run_periodic("files.delete_unreferenced_objects").await;

    assert!(storage.get(&object, None).await.is_err());
}
