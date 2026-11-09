//! The archive of an export that a contract writes, and the unit of work of an export's job
//! (PLATFORM-084, PLATFORM-085).

use std::{
    io::Read,
    sync::{Arc, Mutex},
};

use flate2::read::DeflateDecoder;
use platform::{
    archive::Entry,
    authn::BoxFuture,
    blocking::Workers,
    db::UnitOfWork,
    errors::Internal,
    jobs::JobContext,
    personal_data::{Export, ExportStore, PIECE},
    types::UserId,
};
use serde_json::{Value, json};

use crate::support::{self, TestDatabase};

/// A store that keeps what it is handed, in the order it was handed it.
#[derive(Default)]
struct Kept {
    pieces: Mutex<Vec<(u32, u64, Vec<u8>)>>,
    entries: Mutex<Vec<(u32, Entry)>>,
}

impl ExportStore for Kept {
    fn piece<'a>(
        &'a self,
        _: &'a mut UnitOfWork,
        _: uuid::Uuid,
        position: u32,
        offset: u64,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        self.pieces.lock().unwrap().push((position, offset, bytes));
        Box::pin(async { Ok(()) })
    }

    fn entry<'a>(
        &'a self,
        _: &'a mut UnitOfWork,
        _: uuid::Uuid,
        position: u32,
        entry: Entry,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        self.entries.lock().unwrap().push((position, entry));
        Box::pin(async { Ok(()) })
    }
}

impl Kept {
    /// The deflated bytes of the document at `position`, its pieces joined, after checking that
    /// each starts where the one before ended.
    fn deflated(&self, position: u32) -> Vec<u8> {
        let mut joined = Vec::new();
        for (_, offset, bytes) in self
            .pieces
            .lock()
            .unwrap()
            .iter()
            .filter(|(of, _, _)| *of == position)
        {
            assert_eq!(*offset, joined.len() as u64);
            assert!(bytes.len() <= PIECE);
            joined.extend_from_slice(bytes);
        }
        joined
    }
}

/// `bytes`, inflated.
fn inflated(bytes: &[u8]) -> Vec<u8> {
    let mut text = Vec::new();
    DeflateDecoder::new(bytes).read_to_end(&mut text).unwrap();
    text
}

#[tokio::test]
async fn platform_084_a_document_is_a_json_array_a_record_to_a_line_deflated_in_pieces_of_a_mib() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let mut uow = database.begin(&support::client()).await.unwrap();
    let kept = Arc::new(Kept::default());
    let mut export = Export::new(
        uuid::Uuid::nil(),
        UserId::from_uuid(uuid::Uuid::from_u128(1)),
        kept.clone(),
        Workers::new(),
    );
    // Three pages of records random enough that their deflated bytes pass 2 MiB.
    let pages: Vec<Vec<Value>> = (0..3)
        .map(|page| {
            (0..2_000)
                .map(|index| {
                    let noise: [u8; 500] = platform::random::bytes().unwrap();
                    let noise: String = noise.iter().map(|byte| format!("{byte:02x}")).collect();
                    json!({ "page": page, "index": index, "noise": noise })
                })
                .collect()
        })
        .collect();

    export
        .whole(&mut uow, "empty.json", &[] as &[Value])
        .await
        .unwrap();
    let mut document = export.document("big.json");
    for page in &pages {
        export.write(&mut uow, &mut document, page).await.unwrap();
    }
    export.finish(&mut uow, document).await.unwrap();

    let entries = kept.entries.lock().unwrap().clone();
    assert_eq!(entries.len(), 2);
    let (position, empty) = &entries[0];
    assert_eq!((*position, empty.path.as_str()), (0, "empty.json"));
    assert_eq!(inflated(&kept.deflated(0)), b"[]\n");
    assert_eq!(empty.size, 3);
    let (position, big) = &entries[1];
    assert_eq!((*position, big.path.as_str()), (1, "big.json"));
    let deflated = kept.deflated(1);
    assert!(deflated.len() > 2 * PIECE, "{}", deflated.len());
    let text = inflated(&deflated);
    let lines: Vec<String> = pages
        .iter()
        .flatten()
        .map(|record| serde_json::to_string(record).unwrap())
        .collect();
    let expected = format!("[\n{}\n]\n", lines.join(",\n"));
    assert_eq!(String::from_utf8(text.clone()).unwrap(), expected);
    let parsed: Vec<Value> = serde_json::from_slice(&text).unwrap();
    assert_eq!(parsed.len(), 6_000);
    assert_eq!(big.size, text.len() as u64);
    assert_eq!(big.stored_size, deflated.len() as u64);
    assert_eq!(big.crc32, platform::archive::crc32(&text));
    assert!(big.deflated);
}

#[tokio::test]
async fn platform_085_a_snapshot_job_s_unit_of_work_reads_as_its_first_statement_did() {
    let db = TestDatabase::migrated().await;
    let database = db.app().await;
    let context = JobContext::for_tests();
    let mut snapshot = database.for_snapshot_job(&context).await.unwrap();
    let mut plain = database.for_job(&context).await.unwrap();
    let before = count(&mut snapshot).await;
    let plain_before = count(&mut plain).await;

    db.owner_executes("INSERT INTO workspaces (name) VALUES ('Committed meanwhile')")
        .await;

    assert_eq!(count(&mut snapshot).await, before);
    assert_eq!(count(&mut plain).await, plain_before + 1);
}

/// How many workspaces `uow` reads.
async fn count(uow: &mut UnitOfWork) -> i64 {
    sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM workspaces"#)
        .fetch_one(uow.executor())
        .await
        .unwrap()
}
