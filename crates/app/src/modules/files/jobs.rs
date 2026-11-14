//! The periodic jobs of `files` (OPS-09, FILES-011), owned by `files`, which delete the objects
//! that no row of a file, an avatar, a workspace's picture or a file that an export holds names,
//! once no restore of the database can bring such a row back (the files spec's "Deleting bytes").
//! `files.find_unreferenced_objects`, run every day, lists the bucket's objects under
//! `workspaces/`, `users/`, `avatars/` and `pictures/`, and records each that no row of
//! `workspace_files`, `user_files`, `avatars`, `workspace_pictures` or `exported_files` names, and
//! that the bucket wrote an hour ago or more, in `object_deletions`, with when its bytes may go.
//! `files.delete_unreferenced_objects`, run every hour, deletes from the bucket those of
//! `object_deletions` whose time has passed and that still no row names, then removes them from
//! `object_deletions`. Both touch those six tables alone, and the bucket.
//! Both span every workspace and user by nature, so they select objects by key alone, which
//! `check-tenancy` lets this file and the public reads alone do (DATA-16). Scheduled maintenance
//! isn't audited (DATA-22). A second run of either finds nothing more to do, and a deletion of an
//! object that a run before deleted, whose record it didn't remove, succeeds.

use std::time::Duration;

use futures_util::{StreamExt, TryStreamExt};
use platform::{
    errors::Internal,
    jobs::{Duplicate, JobContext, Jobs},
    storage::ObjectKey,
};

use super::service::{
    Files, USER_PREFIX, WORKSPACE_PREFIX, avatar::AVATAR_PREFIX, picture::PICTURE_PREFIX,
};

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

/// How long ago the bucket must have written an object before the comparison records it: an
/// upload commits the row that names its object within its route's 60 seconds, and the rest of
/// the hour leaves room for the bucket's clock and the database's to differ. An object recorded
/// before its row commits would start its delay before that row's deletion (FILES-011).
const SETTLED: Duration = Duration::from_secs(3_600);

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

/// Records each object of the bucket, under the prefix of workspaces' files, then of users', then
/// of avatars, then of workspaces' pictures, that no row names, and that the bucket wrote at least
/// `SETTLED` ago.
async fn find(files: &Files, context: &JobContext) -> Result<(), Internal> {
    for prefix in [WORKSPACE_PREFIX, USER_PREFIX, AVATAR_PREFIX, PICTURE_PREFIX] {
        find_under(files, context, prefix).await?;
    }
    Ok(())
}

/// Records each object of the bucket under `prefix` that no row names, and that the bucket wrote
/// at least `SETTLED` ago, by the database's clock, a page of the bucket's list at a time, with
/// when its bytes may go: `FILE_DELETION_DELAY_DAYS` from now. An object already recorded keeps
/// its time.
async fn find_under(files: &Files, context: &JobContext, prefix: &str) -> Result<(), Internal> {
    let delay = i32::try_from(files.settings.deletion_delay_days).map_err(Internal::new)?;
    let settled = f64::from(u32::try_from(SETTLED.as_secs()).map_err(Internal::new)?);
    let mut pages = files.storage.list(prefix).try_chunks(BATCH).boxed();
    while let Some(page) = pages.next().await {
        let (keys, written): (Vec<String>, Vec<time::OffsetDateTime>) = page
            .map_err(|error| Internal::new(error.1))?
            .into_iter()
            .map(|listed| (listed.key.as_str().to_owned(), listed.written_at))
            .unzip();
        let mut uow = files.database.for_job(context).await?;
        sqlx::query!(
            "INSERT INTO object_deletions (object_key, deletable_at)
            SELECT key, now() + make_interval(days => $3)
            FROM unnest($1::text[], $2::timestamptz[]) AS listed (key, written_at)
            WHERE written_at <= now() - make_interval(secs => $4)
                AND NOT EXISTS (SELECT 1 FROM workspace_files WHERE object_key = key)
                AND NOT EXISTS (SELECT 1 FROM user_files WHERE object_key = key)
                AND NOT EXISTS (SELECT 1 FROM avatars WHERE object_key = key)
                AND NOT EXISTS (SELECT 1 FROM workspace_pictures WHERE object_key = key)
                AND NOT EXISTS (SELECT 1 FROM exported_files WHERE object_key = key)
            ON CONFLICT (object_key) DO NOTHING",
            &keys,
            &written,
            delay,
            settled,
        )
        .execute(uow.executor())
        .await?;
        uow.commit().await?;
    }
    Ok(())
}

/// Deletes the recorded objects whose time has passed, a batch at a time: from the bucket each
/// that still no row names, then each from `object_deletions`, whichever it did. The batch is read
/// in a transaction that ends before the bucket is asked (DATA-21). A deletion that fails ends the
/// run, keeping what it hadn't removed for the next run.
async fn delete(files: &Files, context: &JobContext) -> Result<(), Internal> {
    loop {
        let limit = i64::try_from(BATCH).map_err(Internal::new)?;
        let mut uow = files.database.for_job(context).await?;
        let due = sqlx::query!(
            r#"SELECT object_key, (
                EXISTS (
                    SELECT 1 FROM workspace_files
                    WHERE workspace_files.object_key = object_deletions.object_key
                ) OR EXISTS (
                    SELECT 1 FROM user_files
                    WHERE user_files.object_key = object_deletions.object_key
                ) OR EXISTS (
                    SELECT 1 FROM avatars
                    WHERE avatars.object_key = object_deletions.object_key
                ) OR EXISTS (
                    SELECT 1 FROM workspace_pictures
                    WHERE workspace_pictures.object_key = object_deletions.object_key
                ) OR EXISTS (
                    SELECT 1 FROM exported_files
                    WHERE exported_files.object_key = object_deletions.object_key
                )
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
