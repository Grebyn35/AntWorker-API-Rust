//! The bucket of files' bytes and its copy (the platform spec's "Storage").

use futures_util::TryStreamExt;
use platform::storage::{Bytes, Held, ObjectKey, Storage};

use crate::support::{TestDatabase, bucket};

/// The keys of every object of the bucket `name`, in the order the bucket lists them.
async fn keys(name: &str) -> Vec<String> {
    bucket::storage(name)
        .list("")
        .map_ok(|key| key.as_str().to_owned())
        .try_collect()
        .await
        .unwrap()
}

/// Writes `key`, with a text's bytes, into `storage`.
async fn put(storage: &Storage, key: &str) {
    storage
        .put(
            &ObjectKey::from_stored(key),
            Bytes::from_static(b"hello"),
            "text/plain; charset=utf-8",
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn platform_078_with_a_copy_put_queues_each_object_and_delete_reaches_both() {
    let db = TestDatabase::migrated().await;
    let (name, copy) = (db.bucket().await, db.copy_bucket().await);
    let storage = bucket::storage_with_copy(&name, &copy);
    let key = "workspaces/0a";

    put(&storage, key).await;

    // The deployment moves the queue to the copy; the API never writes the copy.
    assert_eq!(keys(&name).await, ["copy-queue/workspaces/0a", key]);
    assert!(keys(&copy).await.is_empty());
    // As the deployment's move does: into the copy, out of the queue.
    put(&bucket::storage(&copy), key).await;
    bucket::storage(&name)
        .delete(&ObjectKey::from_stored("copy-queue/workspaces/0a"))
        .await
        .unwrap();

    storage.delete(&ObjectKey::from_stored(key)).await.unwrap();

    assert!(keys(&name).await.is_empty());
    assert!(keys(&copy).await.is_empty());
    // A second deletion finds nothing, and succeeds.
    storage.delete(&ObjectKey::from_stored(key)).await.unwrap();
    // Without a copy, `put` writes once.
    put(&bucket::storage(&name), key).await;
    assert_eq!(keys(&name).await, [key]);
}

#[tokio::test]
async fn platform_078_list_gives_each_key_that_either_bucket_holds_once_in_order() {
    let db = TestDatabase::migrated().await;
    let (name, copy) = (db.bucket().await, db.copy_bucket().await);
    for key in ["workspaces/0a", "workspaces/0b"] {
        put(&bucket::storage(&name), key).await;
    }
    for key in ["workspaces/0b", "workspaces/0c"] {
        put(&bucket::storage(&copy), key).await;
    }
    put(&bucket::storage(&name), "other/0d").await;
    let storage = bucket::storage_with_copy(&name, &copy);

    let held: Vec<(String, Held)> = storage
        .holdings("workspaces/")
        .map_ok(|(key, held)| (key.as_str().to_owned(), held))
        .try_collect()
        .await
        .unwrap();
    let listed: Vec<String> = storage
        .list("workspaces/")
        .map_ok(|key| key.as_str().to_owned())
        .try_collect()
        .await
        .unwrap();
    let alone: Vec<String> = bucket::storage(&name)
        .list("workspaces/")
        .map_ok(|key| key.as_str().to_owned())
        .try_collect()
        .await
        .unwrap();

    assert_eq!(
        held,
        [
            ("workspaces/0a".to_owned(), Held::Bucket),
            ("workspaces/0b".to_owned(), Held::Both),
            ("workspaces/0c".to_owned(), Held::Copy { size: 5 }),
        ]
    );
    assert_eq!(listed, ["workspaces/0a", "workspaces/0b", "workspaces/0c"]);
    assert_eq!(alone, ["workspaces/0a", "workspaces/0b"]);
}

#[tokio::test]
async fn platform_078_the_bucket_is_restoring_while_it_holds_the_mark() {
    let db = TestDatabase::migrated().await;
    let (name, copy) = (db.bucket().await, db.copy_bucket().await);
    let storage = bucket::storage_with_copy(&name, &copy);
    let without_copy = bucket::storage(&name);

    let before = (
        storage.restoring().await.unwrap(),
        storage.is_empty().await.unwrap(),
    );
    storage.mark_restoring().await.unwrap();
    let after = (
        storage.restoring().await.unwrap(),
        storage.is_empty().await.unwrap(),
    );

    assert_eq!(before, (false, true));
    assert_eq!(after, (true, false));
    assert_eq!(keys(&name).await, ["copy-restoring"]);
    // Without a copy, a bucket is never restoring.
    assert!(!without_copy.restoring().await.unwrap());
}
