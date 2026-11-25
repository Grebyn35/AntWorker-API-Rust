//! What an export holds of files, and the checksum each upload records (FILES-025, FILES-026).

use platform::storage::ObjectKey;
use serde_json::json;

use super::{avatars, own::upload_own, upload, workspace_pictures};
use crate::{
    accounts::exports::{built, document, download, entries},
    support::{
        Api, bucket, files,
        users::{self, SignedIn},
        workspaces,
    },
};

/// Downloads the archive of `user`'s export `id`.
async fn archive(api: &Api, user: &SignedIn, id: &str) -> Vec<u8> {
    let response = download(api, user, id, &[]).await;
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
    let avatar = avatars::set(&api, &ada, files::PNG).await;
    let gif = files::GIF;
    let picture = workspace_pictures::set(&api, &ada, &acme, gif).await;

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
    let drafts = workspaces::arrange(db, "Drafts", &[(&ada.user_id, "owner")]).await;
    let shared = workspaces::arrange(
        db,
        "Theirs",
        &[(&grace.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    let own = upload_own(&api, &ada, "a/b: c.txt", b"my own words", "", None)
        .await
        .json();
    let avatar = avatars::set(&api, &ada, files::PNG).await;
    assert_eq!(avatar.status, 200);
    let mine = files::arrange(db, &own_workspace, "notes.md").await;
    let draft = files::arrange(db, &drafts, "draft.md").await;
    // A file Ada uploaded to a workspace that has another member is the workspace's, and so is
    // that workspace's picture.
    files::arrange_uploaded(db, &shared, "shared.txt", &ada.user_id).await;
    let gif = files::GIF;
    let picture = workspace_pictures::set(&api, &ada, &own_workspace, gif).await;
    assert_eq!(picture.status, 200);
    let drafts_picture = workspace_pictures::set(&api, &ada, &drafts, files::PNG).await;
    assert_eq!(drafts_picture.status, 200);
    let theirs = workspace_pictures::set(&api, &grace, &shared, files::PNG).await;
    assert_eq!(theirs.status, 200);

    let id = built(&api, &ada).await;
    let archive = archive(&api, &ada, &id).await;

    let own_id = own["id"].as_str().unwrap();
    let records = document(&archive, "files/files.json");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["name"], "a/b: c.txt");
    assert_eq!(records[0]["path"], format!("files/own/{own_id}/a_b_ c.txt"));
    let avatars = document(&archive, "files/avatar.json");
    let avatar_id = avatars[0]["id"].as_str().unwrap();
    assert_eq!(avatars[0]["path"], format!("files/avatar/{avatar_id}.png"));
    let held = workspaces_held(
        &archive,
        [
            (&own_workspace, &mine, "notes.md", "gif", gif),
            (&drafts, &draft, "draft.md", "png", files::PNG),
        ],
    );
    let bytes: Vec<(String, Vec<u8>)> = entries(&archive)
        .into_iter()
        .filter(|(path, _)| !path.ends_with(".json"))
        .collect();
    let mut expected = vec![
        (
            format!("files/own/{own_id}/a_b_ c.txt"),
            b"my own words".to_vec(),
        ),
        (format!("files/avatar/{avatar_id}.png"), files::PNG.to_vec()),
    ];
    expected.extend(held);
    assert_eq!(bytes, expected);
}

/// A workspace whose only member is the user, as [`workspaces_held`] takes it: its ID, the ID and
/// the name of its one file, and the extension and the bytes of its picture.
type Sole<'a> = (&'a str, &'a str, &'a str, &'a str, &'a [u8]);

/// What `archive` holds of the workspaces `sole`: the records of their files, then those of their
/// pictures, each in the order of the workspaces' IDs; returns the path and the bytes of each such
/// file and picture, in that order.
fn workspaces_held<const N: usize>(
    archive: &[u8],
    mut sole: [Sole<'_>; N],
) -> Vec<(String, Vec<u8>)> {
    sole.sort_unstable_by_key(|(workspace, ..)| *workspace);
    let mut held = Vec::new();
    let records = document(archive, "files/workspace_files.json");
    assert_eq!(records.len(), N);
    for (record, (workspace, file, name, ..)) in records.iter().zip(sole) {
        assert_eq!(record["id"], file);
        assert_eq!(record["workspace_id"], workspace);
        assert_eq!(record["uploader_id"], json!(null));
        let path = format!("files/workspaces/{workspace}/{file}/{name}");
        held.push((path, files::BYTES.to_vec()));
    }
    let pictures = document(archive, "files/workspace_pictures.json");
    assert_eq!(pictures.len(), N);
    for (record, (workspace, _, _, extension, image)) in pictures.iter().zip(sole) {
        let picture_id = record["id"].as_str().unwrap();
        assert_eq!(record["workspace_id"], workspace);
        assert_eq!(record["media_type"], format!("image/{extension}"));
        assert_eq!(record["size_bytes"], image.len());
        assert!(record["created_at"].as_str().unwrap().ends_with('Z'));
        let path = format!("files/workspace_pictures/{workspace}/{picture_id}.{extension}");
        assert_eq!(record["path"], path.as_str());
        held.push((path, image.to_vec()));
    }
    held
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
    let archive = archive(&api, &ada, &id).await;
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
