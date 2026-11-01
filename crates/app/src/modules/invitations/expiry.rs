//! The expiry sweep (INVITATIONS-007): a scheduled job that spans every workspace by nature, so
//! it selects invitations by expiry alone, which `check-tenancy` lets this file alone do
//! (DATA-22). Scheduled maintenance isn't audited (DATA-27).

use platform::{db::Database, errors::Internal, jobs::JobContext};

/// The most invitations one transaction of the sweep deletes.
const BATCH: i64 = 1_000;

/// Deletes the expired invitations of every workspace, a batch at a time.
pub(super) async fn expire(database: &Database, context: &JobContext) -> Result<(), Internal> {
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
