//! The periodic job `invitations.expire` (OPS-09, INVITATIONS-007): owned by `invitations`, run
//! every hour, and touching `invitations` alone. It spans every workspace by nature, so it
//! selects invitations by expiry alone, which `check-tenancy` lets this file alone do (DATA-22).
//! Scheduled maintenance isn't audited (DATA-27). A second run finds nothing more to delete.

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

/// Deletes the expired invitations of every workspace, a batch at a time.
async fn expire(database: &Database, context: &JobContext) -> Result<(), Internal> {
    loop {
        let mut uow = database.for_job(context).await?;
        let deleted = sqlx::query!(
            "DELETE FROM invitations WHERE id IN (
                SELECT id FROM invitations WHERE expires_at <= now() LIMIT $1
            )",
            BATCH,
        )
        .execute(uow.executor())
        .await?
        .rows_affected();
        uow.commit().await?;
        if deleted < BATCH.unsigned_abs() {
            return Ok(());
        }
    }
}
