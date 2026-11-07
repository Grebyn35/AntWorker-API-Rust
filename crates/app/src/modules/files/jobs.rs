//! The periodic jobs of `files` (OPS-09, FILES-011), which delete the objects that no file's row
//! names, once no restore of the database can bring such a row back (the files spec's "Deleting
//! bytes"), from the bucket and from its copy, when it has one (PLATFORM-078):
//! `files.find_unreferenced_objects` records each such object that either holds with when its
//! bytes may go, and `files.delete_unreferenced_objects` deletes those whose time has passed and
//! that still no row names. Both span every workspace by nature, so they select objects by key alone, which
//! `check-tenancy` lets this file alone do (DATA-16). Scheduled maintenance isn't audited
//! (DATA-22). A second run of either finds nothing more to do.

use std::time::Duration;

use futures_util::{StreamExt, TryStreamExt};
use platform::{
    errors::Internal,
    jobs::{Duplicate, JobContext, Jobs},
    storage::ObjectKey,
};

use super::service::{Files, WORKSPACE_PREFIX};

/// How often the bucket is compared with the rows.
const FIND_INTERVAL: Duration = Duration::from_secs(86_400);

/// How long one comparison may take: a page of the bucket's list a request, and one statement a
/// page.
const FIND_TIME_LIMIT: Duration = Duration::from_secs(3_600);

/// How often the recorded objects whose time has passed are deleted.
const DELETE_INTERVAL: Duration = Duration::from_secs(3_600);

/// How long one deletion run may take: a request to the bucket for each object it deletes.
const DELETE_TIME_LIMIT: Duration = Duration::from_secs(900);

/// The most objects one statement records, or one batch deletes.
const BATCH: usize = 1_000;

impl Files {
    /// Registers the two jobs.
    pub(crate) fn register(&self, jobs: &mut Jobs) -> Result<(), Duplicate> {
        let files = self.clone();
        jobs.periodic(
            "files.find_unreferenced_objects",
            FIND_INTERVAL,
            FIND_TIME_LIMIT,
            move |context| {
                let files = files.clone();
                async move { find(&files, &context).await }
            },
        )?;
        let files = self.clone();
        jobs.periodic(
            "files.delete_unreferenced_objects",
            DELETE_INTERVAL,
            DELETE_TIME_LIMIT,
            move |context| {
                let files = files.clone();
                async move { delete(&files, &context).await }
            },
        )
    }
}

/// Records each object of the bucket or its copy that no row names, a page of their merged list at
/// a time, with when its bytes may go: `FILE_DELETION_DELAY_DAYS` from now, by the database's
/// clock. An object already recorded keeps its time.
async fn find(files: &Files, context: &JobContext) -> Result<(), Internal> {
    let delay = i32::try_from(files.settings.deletion_delay_days).map_err(Internal::new)?;
    let mut pages = files
        .storage
        .list(WORKSPACE_PREFIX)
        .try_chunks(BATCH)
        .boxed();
    while let Some(page) = pages.next().await {
        let keys: Vec<String> = page
            .map_err(|error| Internal::new(error.1))?
            .into_iter()
            .map(|key| key.as_str().to_owned())
            .collect();
        let mut uow = files.database.for_job(context).await?;
        sqlx::query!(
            "INSERT INTO object_deletions (object_key, deletable_at)
            SELECT key, now() + make_interval(days => $2) FROM unnest($1::text[]) AS key
            WHERE NOT EXISTS (SELECT 1 FROM workspace_files WHERE object_key = key)
            ON CONFLICT (object_key) DO NOTHING",
            &keys,
            delay,
        )
        .execute(uow.executor())
        .await?;
        uow.commit().await?;
    }
    Ok(())
}

/// Deletes the recorded objects whose time has passed, a batch at a time: from the copy, then the
/// bucket, each that still no row names, then each from `object_deletions`, whichever it did. The batch is read
/// in a transaction that ends before the bucket is asked (DATA-21). A deletion that fails ends the
/// run, keeping what it hadn't removed for the next run.
async fn delete(files: &Files, context: &JobContext) -> Result<(), Internal> {
    loop {
        let limit = i64::try_from(BATCH).map_err(Internal::new)?;
        let mut uow = files.database.for_job(context).await?;
        let due = sqlx::query!(
            r#"SELECT object_key, EXISTS (
                SELECT 1 FROM workspace_files WHERE workspace_files.object_key = object_deletions.object_key
            ) AS "named!"
            FROM object_deletions WHERE deletable_at <= now()
            ORDER BY deletable_at LIMIT $1"#,
            limit,
        )
        .fetch_all(uow.executor())
        .await?;
        uow.commit().await?;
        let mut done = Vec::with_capacity(due.len());
        let mut failure = None;
        for object in &due {
            if !object.named
                && let Err(error) = files
                    .storage
                    .delete(&ObjectKey::from_stored(&object.object_key))
                    .await
            {
                failure = Some(Internal::new(error));
                break;
            }
            done.push(object.object_key.clone());
        }
        let mut uow = files.database.for_job(context).await?;
        sqlx::query!(
            "DELETE FROM object_deletions WHERE object_key = ANY($1)",
            &done,
        )
        .execute(uow.executor())
        .await?;
        uow.commit().await?;
        if let Some(error) = failure {
            return Err(error);
        }
        if due.len() < BATCH {
            return Ok(());
        }
    }
}
