//! Builders of files, through the `owner` connection and the test's bucket (TEST-05, TEST-09).

use bytes::Bytes;
use platform::storage::ObjectKey;

use super::{TestDatabase, bucket};

/// The bytes of every file a builder arranges: a text file's.
pub const BYTES: &[u8] = b"hello";

/// The CRC-32 of [`BYTES`], which the row of every file a builder arranges records (FILES-025).
pub const CRC32: u32 = 907_060_870;

/// Creates a text file named `name` in `workspace`, with [`BYTES`] as its object in the test's
/// bucket, uploaded by nobody, and returns its ID.
pub async fn arrange(db: &TestDatabase, workspace: &str, name: &str) -> String {
    let key = ObjectKey::random("workspaces/").unwrap();
    bucket::storage(&db.bucket().await)
        .put(&key, Bytes::from_static(BYTES), "text/plain; charset=utf-8")
        .await
        .unwrap();
    let name = name.replace('\'', "''");
    db.owner_reads(&format!(
        "INSERT INTO workspace_files \
         (workspace_id, name, media_type, size_bytes, crc32, visibility, object_key) \
         VALUES ('{workspace}', '{name}', 'text/plain; charset=utf-8', {}, {CRC32}, 'private', \
         '{}') RETURNING id::text",
        BYTES.len(),
        key.as_str()
    ))
    .await
    .remove(0)
}

/// Creates a text file named `name` in `workspace`, as [`arrange`] does, uploaded by `uploader`,
/// and returns its ID.
pub async fn arrange_uploaded(
    db: &TestDatabase,
    workspace: &str,
    name: &str,
    uploader: &str,
) -> String {
    let id = arrange(db, workspace, name).await;
    db.owner_executes(&format!(
        "UPDATE workspace_files SET uploader_id = '{uploader}' WHERE id = '{id}'"
    ))
    .await;
    id
}

/// Makes a PNG image, whose object the bucket doesn't hold, `user`'s avatar, and returns its ID.
pub async fn arrange_avatar(db: &TestDatabase, user: &str) -> String {
    let key = ObjectKey::random("avatars/").unwrap();
    db.owner_reads(&format!(
        "INSERT INTO avatars (user_id, media_type, size_bytes, crc32, object_key) \
         VALUES ('{user}', 'image/png', 5, 0, '{}') RETURNING id::text",
        key.as_str()
    ))
    .await
    .remove(0)
}

/// Creates a text file named `name` in `workspace` whose object the bucket doesn't hold, uploaded
/// by nobody, and returns its ID, for a test whose bucket can't be reached.
pub async fn arrange_row(db: &TestDatabase, workspace: &str, name: &str) -> String {
    let key = ObjectKey::random("workspaces/").unwrap();
    let name = name.replace('\'', "''");
    db.owner_reads(&format!(
        "INSERT INTO workspace_files \
         (workspace_id, name, media_type, size_bytes, crc32, visibility, object_key) \
         VALUES ('{workspace}', '{name}', 'text/plain; charset=utf-8', {}, {CRC32}, 'private', \
         '{}') RETURNING id::text",
        BYTES.len(),
        key.as_str()
    ))
    .await
    .remove(0)
}

/// Creates a text file named `name` in `user`'s own files, with [`BYTES`] as its object in the
/// test's bucket, and returns its ID.
pub async fn arrange_own(db: &TestDatabase, user: &str, name: &str) -> String {
    let key = ObjectKey::random("users/").unwrap();
    bucket::storage(&db.bucket().await)
        .put(&key, Bytes::from_static(BYTES), "text/plain; charset=utf-8")
        .await
        .unwrap();
    let name = name.replace('\'', "''");
    db.owner_reads(&format!(
        "INSERT INTO user_files \
         (user_id, name, media_type, size_bytes, crc32, visibility, object_key) \
         VALUES ('{user}', '{name}', 'text/plain; charset=utf-8', {}, {CRC32}, 'private', '{}') \
         RETURNING id::text",
        BYTES.len(),
        key.as_str()
    ))
    .await
    .remove(0)
}

/// Makes the file `id` of `table`, `workspace_files` or `user_files`, a public one.
pub async fn publish(db: &TestDatabase, table: &str, id: &str) {
    db.owner_executes(&format!(
        "UPDATE {table} SET visibility = 'public' WHERE id = '{id}'"
    ))
    .await;
}
