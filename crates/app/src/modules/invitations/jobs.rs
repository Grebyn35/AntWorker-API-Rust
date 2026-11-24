//! The periodic job `invitations.expire` (OPS-09, INVITATIONS-007): owned by `invitations`, run
//! every hour, and touching `invitations` alone. It spans every workspace by nature, so it
//! selects invitations by expiry alone, which DATA-16 exempts, and `check-tenancy` lets `expire`
//! do, as a function that it lists. Scheduled maintenance isn't audited (DATA-22). A run deletes
//! batches, the longest expired first, until one deletes nothing, so a run right after one whose
//! last batch selected no invitation finds nothing more to delete, and an invitation that a resend
//! or a refresh renewed while the sweep waited for its lock stays (DATA-20, INVITATIONS-015).

use std::time::Duration;

use platform::{
    db::Database,
    errors::Internal,
    jobs::{Duplicate, JobContext, Jobs},
};

use super::service::Invitations;

/// How often the sweep runs.
const INTERVAL: Duration = Duration::from_secs(3_600);

/// How long one run of the sweep may take.
const TIME_LIMIT: Duration = Duration::from_secs(300);

/// The most invitations one transaction of the sweep deletes.
const BATCH: i64 = 1_000;

impl Invitations {
    /// Registers the sweep.
    pub(crate) fn register(&self, jobs: &mut Jobs) -> Result<(), Duplicate> {
        let database = self.database.clone();
        jobs.periodic("invitations.expire", INTERVAL, TIME_LIMIT, move |context| {
            let database = database.clone();
            async move { expire(&database, &context).await }
        })
    }
}

/// Deletes the expired invitations of every workspace, a batch at a time, the longest expired
/// first, until a batch deletes none. A batch some of whose invitations a renewal, or another
/// deletion, took while it waited ends no run if it deletes one; one whose every invitation they
/// took deletes none, and ends the run, though others may still be expired until the next run.
async fn expire(database: &Database, context: &JobContext) -> Result<(), Internal> {
    loop {
        let mut uow = database.for_job(context).await?;
        // The outer test of the expiry is read again on a row that changed while the statement
        // waited for its lock, which the subquery's rows aren't (DATA-20).
        let deleted = sqlx::query!(
            "DELETE FROM invitations WHERE id IN (
                SELECT id FROM invitations WHERE expires_at <= now() ORDER BY expires_at LIMIT $1
            ) AND expires_at <= now()",
            BATCH,
        )
        .execute(uow.executor())
        .await?
        .rows_affected();
        uow.commit().await?;
        if deleted == 0 {
            return Ok(());
        }
    }
}
