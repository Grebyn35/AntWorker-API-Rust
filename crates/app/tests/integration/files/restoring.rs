//! The operator's writing back of the files' bytes that were deleted or replaced in their bucket
//! since a moment, from the versions that the bucket kept (the files spec's "Restoring bytes",
//! FILES-028, FILES-029).

use std::process::Output;

use bytes::Bytes;
use futures_util::{StreamExt, TryStreamExt};
use platform::{
    storage::{ObjectKey, Storage, StorageError},
    types::Timestamp,
};

use crate::support::{
    Api,
    bucket::{self, Kept, Written},
    operator, run_binary,
};

/// The moment of the tests of a stand-in bucket, whose versions were written in its minute.
const MOMENT: &str = "2026-11-20T12:00:30Z";

/// When a stand-in bucket wrote a version: at the second `second` of the moment's minute.
fn at(second: u8) -> &'static str {
    match second {
        5 => "2026-11-20T12:00:05.000Z",
        10 => "2026-11-20T12:00:10.000Z",
        20 => "2026-11-20T12:00:20.000Z",
        40 => "2026-11-20T12:00:40.000Z",
        45 => "2026-11-20T12:00:45.000Z",
        _ => "2026-11-20T12:00:50.000Z",
    }
}

/// Runs the binary's `restore-files` with `arguments` against the bucket `name` at `endpoint`,
/// with the settings that the subcommand reads, and no other.
async fn restore(endpoint: &str, name: &str, arguments: &[&str]) -> Output {
    let arguments: Vec<&str> = ["restore-files"]
        .into_iter()
        .chain(arguments.iter().copied())
        .collect();
    run_binary(
        &arguments,
        &[
            ("ENVIRONMENT", "test"),
            ("S3_ENDPOINT", endpoint),
            ("S3_REGION", "us-east-1"),
            ("S3_BUCKET", name),
            ("S3_URI_STYLE", "path"),
            ("S3_ACCESS_KEY_ID", "test"),
            ("S3_SECRET_ACCESS_KEY", "test"),
        ],
    )
    .await
}

/// Asserts that `output` exited 0 with the one line that says what the run did: `outcome`.
fn finished(output: &Output, outcome: &str) {
    operator::finished(output, "restore-files", outcome, &[]);
}

/// The bytes of the object `key`, as `storage`'s bucket holds it now.
async fn held(storage: &Storage, key: &ObjectKey) -> Result<Vec<u8>, StorageError> {
    let mut object = storage.get(key, None).await?;
    let mut bytes = Vec::new();
    while let Some(chunk) = object.bytes.next().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    Ok(bytes)
}

/// What a stand-in bucket was asked to write, as the key and the text of each write.
fn writes(written: &[Written]) -> Vec<(&str, String)> {
    written
        .iter()
        .map(|write| {
            (
                write.key.as_str(),
                String::from_utf8_lossy(&write.bytes).into_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn files_028_the_subcommand_writes_again_what_was_deleted_or_replaced_since_the_moment() {
    let api = Api::new().await;
    let name = api.database().bucket().await;
    bucket::keep_versions(&name).await;
    let storage = bucket::storage(&name);
    let key = |key: &str| ObjectKey::from_stored(key);
    let (deleted, replaced, untouched) = (key("workspaces/aa"), key("users/bb"), key("avatars/cc"));
    let (later, outside) = (key("pictures/dd"), key("server-check/probe"));
    let first = Bytes::from_static(b"first");
    for (object, media_type) in [
        (&deleted, "image/png"),
        (&replaced, "text/plain"),
        (&untouched, "text/plain"),
        (&outside, "text/plain"),
    ] {
        storage
            .put(object, first.clone(), media_type)
            .await
            .unwrap();
    }
    let before: Vec<_> = storage.versions("").try_collect().await.unwrap();
    let moment = before.iter().map(|version| version.written_at).max();
    let moment = Timestamp::from_database(moment.unwrap()).to_string();
    // The bucket's times are whole seconds, and a version of the moment's second counts as
    // written before it: what follows is stamped later.
    bucket::next_second().await;
    storage.delete(&deleted).await.unwrap();
    let second = Bytes::from_static(b"second, of another size");
    storage.put(&replaced, second, "text/plain").await.unwrap();
    storage
        .put(&later, first.clone(), "image/png")
        .await
        .unwrap();
    storage.delete(&later).await.unwrap();
    storage.delete(&outside).await.unwrap();

    let ran = restore(bucket::ENDPOINT, &name, &[&moment]).await;
    let after: Vec<_> = storage.versions("").try_collect().await.unwrap();
    let again = restore(bucket::ENDPOINT, &name, &[&moment]).await;

    finished(&ran, "objects seen 4, written again 2, left as they are 0");
    assert_eq!(held(&storage, &deleted).await.unwrap(), b"first");
    assert_eq!(held(&storage, &replaced).await.unwrap(), b"first");
    assert_eq!(held(&storage, &untouched).await.unwrap(), b"first");
    for gone in [&later, &outside] {
        assert!(matches!(
            held(&storage, gone).await,
            Err(StorageError::NotFound)
        ));
    }
    // The object written again holds the media type of its version of then, and each of the
    // run's two writes added one version to those that the bucket kept, deleting none.
    let latest = after
        .iter()
        .find(|version| version.key == deleted && version.latest)
        .unwrap();
    let kept = storage.version(&deleted, &latest.id).await.unwrap();
    assert_eq!(kept.media_type, "image/png");
    let count = |versions: &[platform::storage::Version], object: &ObjectKey| {
        versions
            .iter()
            .filter(|version| &version.key == object)
            .count()
    };
    assert_eq!(count(&after, &deleted), 3);
    assert_eq!(count(&after, &replaced), 3);
    assert_eq!(count(&after, &untouched), 1);
    assert_eq!(after.len(), 11);
    // A second run finds each object's current version with the bytes of its version of then.
    finished(
        &again,
        "objects seen 4, written again 0, left as they are 0",
    );
    let last: Vec<_> = storage.versions("").try_collect().await.unwrap();
    assert_eq!(last.len(), after.len());
}

#[tokio::test]
async fn files_028_each_object_is_written_from_its_version_at_the_moment_page_after_page() {
    let kept = vec![
        // Outside the four prefixes, deleted since the moment.
        Kept::marker("other/gg", "2", true, at(45)),
        Kept::version("other/gg", "1", false, at(20), b"other"),
        // Deleted since the moment.
        Kept::marker("workspaces/aa", "2", true, at(40)),
        Kept::version("workspaces/aa", "1", false, at(20), b"first"),
        // Replaced twice since the moment, and once before it.
        Kept::version("workspaces/bb", "4", true, at(50), b"fourth"),
        Kept::version("workspaces/bb", "3", false, at(40), b"third"),
        Kept::version("workspaces/bb", "2", false, at(20), b"second"),
        Kept::version("workspaces/bb", "1", false, at(10), b"first"),
        // Unchanged since the moment.
        Kept::version("workspaces/cc", "1", true, at(20), b"kept"),
        // Deleted before the moment.
        Kept::marker("users/dd", "2", true, at(10)),
        Kept::version("users/dd", "1", false, at(5), b"gone"),
        // Written since the moment.
        Kept::version("avatars/ee", "1", true, at(40), b"new"),
        // Deleted since the moment, then written with other bytes, then deleted.
        Kept::marker("pictures/ff", "4", true, at(50)),
        Kept::version("pictures/ff", "3", false, at(45), b"other"),
        Kept::marker("pictures/ff", "2", false, at(40)),
        Kept::version("pictures/ff", "1", false, at(20), b"picture"),
    ];
    // Two versions a page, so the versions of one object come on up to three pages.
    let bucket = bucket::keeping(kept, 2, None).await;

    let ran = restore(&bucket.endpoint, "files", &[MOMENT]).await;

    finished(&ran, "objects seen 6, written again 3, left as they are 0");
    let written = bucket.written();
    assert_eq!(
        writes(&written),
        [
            ("workspaces/aa", "first".to_owned()),
            ("workspaces/bb", "second".to_owned()),
            ("pictures/ff", "picture".to_owned()),
        ]
    );
    assert!(written.iter().all(|write| write.media_type == "text/plain"));
}

#[tokio::test]
async fn files_028_an_object_that_the_api_can_t_have_written_is_left_and_counted() {
    // The bytes of the largest object that the API writes, which the stand-in holds for as long
    // as the test's process runs, and an object that the listing gives one byte more, whose
    // bytes no run reads.
    let largest: &'static [u8] = Box::leak(vec![b'a'; 20_971_520].into_boxed_slice());
    let kept = vec![
        Kept::marker("users/large", "2", true, at(40)),
        Kept::version("users/large", "1", false, at(20), b"no bytes read").listed_at(20_971_521),
        Kept::marker("users/largest", "2", true, at(40)),
        Kept::version("users/largest", "1", false, at(20), largest),
        // A key with an empty segment, which no request's URL names.
        Kept::marker("users//unnamed", "2", true, at(40)),
        Kept::version("users//unnamed", "1", false, at(20), b"unnamed"),
    ];
    let bucket = bucket::keeping(kept, 1_000, None).await;

    let ran = restore(&bucket.endpoint, "files", &[MOMENT]).await;

    finished(&ran, "objects seen 3, written again 1, left as they are 2");
    let written = bucket.written();
    assert_eq!(written.len(), 1);
    assert_eq!(written[0].key, "users/largest");
    assert_eq!(written[0].bytes.len(), 20_971_520);
}

#[tokio::test]
async fn files_029_a_run_that_stops_exits_1_and_says_how_many_objects_it_had_written() {
    let deleted = |key: &'static str| {
        [
            Kept::marker(key, "2", true, at(40)),
            Kept::version(key, "1", false, at(20), b"first"),
        ]
    };
    let three: Vec<Kept> = ["workspaces/aa", "workspaces/bb", "users/cc"]
        .into_iter()
        .flat_map(deleted)
        .collect();
    // The bucket takes the first write, and refuses the second.
    let refusing = bucket::keeping(three, 1_000, Some(1)).await;
    // A listing whose first version of an object isn't its latest, after one that is written.
    let unordered = bucket::keeping(
        deleted("workspaces/aa")
            .into_iter()
            .chain([
                Kept::version("workspaces/bb", "1", false, at(20), b"first"),
                Kept::marker("workspaces/bb", "2", true, at(40)),
            ])
            .collect(),
        1_000,
        None,
    )
    .await;
    // A version whose bytes are fewer than the listing says.
    let short = bucket::keeping(
        vec![
            Kept::marker("workspaces/aa", "2", true, at(40)),
            Kept::version("workspaces/aa", "1", false, at(20), b"first").listed_at(6),
        ],
        1_000,
        None,
    )
    .await;

    let refused = restore(&refusing.endpoint, "files", &[MOMENT]).await;
    let disordered = restore(&unordered.endpoint, "files", &[MOMENT]).await;
    let cut = restore(&short.endpoint, "files", &[MOMENT]).await;

    let stopped = |output: &Output, written: u8, cause: &str| {
        let message = format!("stopped once it had written {written} objects again");
        operator::refused(output, "restore-files", &message, &[]);
        operator::refused(output, "restore-files", cause, &[]);
    };
    stopped(&refused, 1, "the bucket answered S3 PutObject with 500");
    assert_eq!(
        writes(&refusing.written()),
        [
            ("workspaces/aa", "first".to_owned()),
            ("workspaces/bb", "first".to_owned()),
        ]
    );
    stopped(
        &disordered,
        1,
        "in another order than S3's, the latest first",
    );
    assert_eq!(writes(&unordered.written()).len(), 1);
    stopped(&cut, 0, "at another size than it listed them at");
    assert!(short.written().is_empty());
}

#[tokio::test]
async fn files_029_what_isn_t_a_moment_as_the_api_writes_one_is_refused_before_any_request() {
    let bucket = bucket::scripted(Some(500)).await;

    for moment in [
        "yesterday",
        "2026-10-25 10:30:00+00",
        "2026-10-25T10:30:00+00:00",
        "2026-10-25T10:30:00",
        "2026-02-30T10:30:00Z",
        "",
    ] {
        let refused = restore(&bucket.endpoint, "files", &[moment]).await;
        operator::refused(
            &refused,
            "restore-files",
            "the moment given isn't one, written YYYY-MM-DDTHH:MM:SSZ, in UTC",
            &[],
        );
    }
    let none = restore(&bucket.endpoint, "files", &[]).await;
    let two = restore(&bucket.endpoint, "files", &[MOMENT, MOMENT]).await;

    for unparsed in [none, two] {
        assert_eq!(unparsed.status.code(), Some(2), "{unparsed:?}");
    }
    assert_eq!(bucket.paths(), Vec::<String>::new());
}
