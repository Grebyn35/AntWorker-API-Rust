//! Builders of files, through the `owner` connection and the test's bucket (TEST-05, TEST-09).

use bytes::Bytes;
use platform::storage::ObjectKey;

use super::{TestDatabase, bucket};

/// The bytes of every file a builder arranges: a text file's.
pub const BYTES: &[u8] = b"hello";

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
        "INSERT INTO workspace_files (workspace_id, name, media_type, size_bytes, object_key) \
         VALUES ('{workspace}', '{name}', 'text/plain; charset=utf-8', {}, '{}') \
         RETURNING id::text",
        BYTES.len(),
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
        "INSERT INTO workspace_files (workspace_id, name, media_type, size_bytes, object_key) \
         VALUES ('{workspace}', '{name}', 'text/plain; charset=utf-8', {}, '{}') \
         RETURNING id::text",
        BYTES.len(),
        key.as_str()
    ))
    .await
    .remove(0)
}
