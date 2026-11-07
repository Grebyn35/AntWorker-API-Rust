//! `restore-files`, an operator's task that a restore runs once the database is restored, and a
//! deploy runs with `--when-empty` before its replicas start (FILES-015, the files spec's
//! "Restoring from the copy"). It reads the rows a page at a time, in the order of their keys,
//! beside the merged lists of the bucket and its copy, in the same order, so it holds no whole list
//! in memory, and no transaction stays open while a bucket is asked (DATA-21). It spans every
//! workspace by nature, so it selects rows by their keys alone, which `check-tenancy` lets this
//! file do (DATA-16).
#![expect(
    clippy::disallowed_methods,
    reason = "restore-files is an operator's task, which opens units of work of its own and records with no actor (the files spec)"
)]

use std::collections::BTreeMap;

use futures_util::StreamExt;
use platform::{
    authn::AuditScope,
    db::Database,
    errors::Internal,
    storage::{Held, Storage},
    types::WorkspaceId,
};

use super::{audit, service::WORKSPACE_PREFIX};
use crate::modules::audit::{self as log, Entry};

/// The most rows one statement reads, or deletes.
const PAGE: usize = 1_000;

/// What `restore-files` did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Restored {
    /// With `--when-empty`, the bucket held an object, so nothing was read.
    NotEmpty,
    /// The files it removed, counted by workspace, and whether it marked the bucket as restoring
    /// from its copy.
    Checked {
        lost: BTreeMap<WorkspaceId, u64>,
        restoring: bool,
    },
}

/// A row, by the key of its object.
struct Row {
    id: uuid::Uuid,
    workspace_id: uuid::Uuid,
    object_key: String,
}

/// What the lists of the two buckets have shown so far.
#[derive(Default)]
struct Seen {
    /// The copy holds an object.
    copy_holds_one: bool,
    /// The copy holds an object that the bucket doesn't.
    copy_alone: bool,
}

impl Seen {
    fn note(&mut self, held: Held) {
        match held {
            Held::Bucket => {}
            Held::Both => self.copy_holds_one = true,
            Held::Copy => {
                self.copy_holds_one = true;
                self.copy_alone = true;
            }
        }
    }
}

/// Removes each file whose object neither the bucket nor its copy holds, with its entry
/// `file.lost` in its workspace's log, and marks the bucket as restoring from its copy when the
/// copy holds an object that the bucket doesn't (FILES-015). With `when_empty`, it does nothing
/// while the bucket holds an object. It refuses to remove a file while the copy holds no object
/// at all, as when its settings name a new bucket, rather than remove every file the bucket
/// lacks.
pub(crate) async fn restore(
    database: &Database,
    storage: &Storage,
    when_empty: bool,
) -> Result<Restored, Internal> {
    if when_empty && !storage.is_empty().await.map_err(Internal::new)? {
        return Ok(Restored::NotEmpty);
    }
    let mut holdings = storage.holdings(WORKSPACE_PREFIX);
    let mut next = holdings.next().await.transpose().map_err(Internal::new)?;
    let mut seen = Seen::default();
    let mut lost = Vec::new();
    let mut after = None;
    loop {
        let rows = page(database, after.as_deref()).await?;
        for row in &rows {
            while let Some((key, held)) = &next
                && key.as_str() < row.object_key.as_str()
            {
                seen.note(*held);
                next = holdings.next().await.transpose().map_err(Internal::new)?;
            }
            match &next {
                Some((key, held)) if key.as_str() == row.object_key => {
                    seen.note(*held);
                    next = holdings.next().await.transpose().map_err(Internal::new)?;
                }
                _ => lost.push((row.id, WorkspaceId::from_uuid(row.workspace_id))),
            }
        }
        if rows.len() < PAGE {
            break;
        }
        after = rows.last().map(|row| row.object_key.clone());
    }
    while let Some((_, held)) = next {
        seen.note(held);
        next = holdings.next().await.transpose().map_err(Internal::new)?;
    }
    if !lost.is_empty() && !seen.copy_holds_one {
        return Err(Internal::new(format!(
            "the copy holds no object, while {} files' rows name objects that the bucket doesn't \
             hold either: S3_COPY_BUCKET may name another bucket, so no file was removed",
            lost.len()
        )));
    }
    let lost = remove(database, &lost).await?;
    if seen.copy_alone {
        storage.mark_restoring().await.map_err(Internal::new)?;
    }
    Ok(Restored::Checked {
        lost,
        restoring: seen.copy_alone,
    })
}

/// The rows whose keys follow `after`, or every row's from the first, in the order of the keys,
/// which the column's collation, "C", keeps to the bytes' order, as a bucket lists them.
async fn page(database: &Database, after: Option<&str>) -> Result<Vec<Row>, Internal> {
    let limit = i64::try_from(PAGE).map_err(Internal::new)?;
    let mut uow = database.for_task().await?;
    let rows = sqlx::query_as!(
        Row,
        "SELECT id, workspace_id, object_key FROM workspace_files
        WHERE object_key > COALESCE($1, '') ORDER BY object_key LIMIT $2",
        after,
        limit,
    )
    .fetch_all(uow.executor())
    .await?;
    uow.commit().await?;
    Ok(rows)
}

/// Deletes the rows of `lost`, a batch at a time, each with its entry `file.lost` in its
/// workspace's log, which no actor records, and counts by workspace those it deleted: a row that a
/// member deleted meanwhile is neither counted nor recorded again.
async fn remove(
    database: &Database,
    lost: &[(uuid::Uuid, WorkspaceId)],
) -> Result<BTreeMap<WorkspaceId, u64>, Internal> {
    let mut counted = BTreeMap::new();
    for batch in lost.chunks(PAGE) {
        let ids: Vec<uuid::Uuid> = batch.iter().map(|(id, _)| *id).collect();
        let mut uow = database.for_task().await?;
        let deleted = sqlx::query!(
            "DELETE FROM workspace_files WHERE id = ANY($1) RETURNING id, workspace_id",
            &ids,
        )
        .fetch_all(uow.executor())
        .await?;
        for row in deleted {
            let workspace = WorkspaceId::from_uuid(row.workspace_id);
            let entry = Entry::new(audit::FILE_LOST, audit::FILE, row.id);
            log::record(&mut uow, AuditScope::unattended(workspace), entry)
                .await
                .map_err(|error| match error {
                    crate::modules::audit::AuditError::Internal(error) => error,
                })?;
            *counted.entry(workspace).or_insert(0) += 1;
        }
        uow.commit().await?;
    }
    Ok(counted)
}
