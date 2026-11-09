//! The archive of an export that a contract writes, and the unit of work of an export's job
//! (PLATFORM-084, PLATFORM-085).

use std::io::Read;

use flate2::read::DeflateDecoder;
use platform::{db::UnitOfWork, jobs::JobContext, personal_data::CHUNK};
use serde_json::Value;

use crate::{
    accounts::exports::built,
    support::{Api, TestDatabase, audit, users, workspaces},
};

/// The bytes of `hex`, which Postgres's `encode(..., 'hex')` wrote.
fn unhex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect()
}

/// The CRC-32 of `bytes`.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(bytes);
    crc.sum()
}

/// `bytes`, inflated.
fn inflated(bytes: &[u8]) -> Vec<u8> {
    let mut text = Vec::new();
    DeflateDecoder::new(bytes).read_to_end(&mut text).unwrap();
    text
}

/// The entry of the document at `path` of the one export, as `size stored crc32`, with the chunks
/// its store kept, each with its offset, in their order.
async fn stored(db: &TestDatabase, path: &str) -> (String, Vec<(u64, Vec<u8>)>) {
    let entry = db
        .owner_reads(&format!(
            "SELECT size_bytes || ' ' || stored_bytes || ' ' || crc32 FROM export_entries \
             WHERE path = '{path}'"
        ))
        .await
        .remove(0);
    let chunks = db
        .owner_reads(&format!(
            "SELECT chunk.offset_bytes || ' ' || encode(chunk.bytes, 'hex') \
             FROM export_chunks AS chunk JOIN export_entries AS entry \
                 ON entry.export_id = chunk.export_id AND entry.position = chunk.position \
             WHERE entry.path = '{path}' ORDER BY chunk.offset_bytes"
        ))
        .await
        .into_iter()
        .map(|row| {
            let (offset, hex) = row.split_once(' ').unwrap();
            (offset.parse().unwrap(), unhex(hex))
        })
        .collect();
    (entry, chunks)
}

#[tokio::test]
async fn platform_084_a_document_is_a_json_array_a_record_to_a_line_deflated_in_chunks_of_a_mib() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // Entries of Ada's own log, random enough that the log's deflated bytes pass 2 MiB.
    audit::noise(api.database(), &ada.user_id, "tests.noise", 6_000).await;

    built(&api, &ada).await;

    let (entry, chunks) = stored(api.database(), "audit/log.json").await;
    let mut deflated = Vec::new();
    for (offset, bytes) in &chunks {
        assert_eq!(*offset, deflated.len() as u64);
        assert!(bytes.len() <= CHUNK);
        deflated.extend_from_slice(bytes);
    }
    assert!(chunks.len() >= 3, "{}", chunks.len());
    let text = inflated(&deflated);
    assert_eq!(
        entry,
        format!("{} {} {}", text.len(), deflated.len(), crc32(&text))
    );
    // `[`, then each record on a line of its own, each but the last followed by `,`, then `]`:
    // within a record, a JSON string holds no raw line feed.
    let text = String::from_utf8(text).unwrap();
    let inner = text
        .strip_prefix("[\n")
        .and_then(|rest| rest.strip_suffix("\n]\n"))
        .unwrap();
    let records: Vec<Value> = inner
        .split(",\n")
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records.iter().all(Value::is_object));
    assert!(records.len() > 6_000);
    // A document of no records: Ada belongs to no workspace.
    let (entry, chunks) = stored(api.database(), "workspaces/workspaces.json").await;
    let deflated: Vec<u8> = chunks.into_iter().flat_map(|(_, bytes)| bytes).collect();
    assert_eq!(inflated(&deflated), b"[]\n");
    assert!(entry.starts_with("3 "), "{entry}");
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

    workspaces::arrange(&db, "Committed meanwhile", &[]).await;

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
