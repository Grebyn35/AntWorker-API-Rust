//! The SQL of a change of a workspace's picture, whose row `files` keeps: the workspace's own
//! part of it, the move of its version (the workspaces spec's "The picture").

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{Version, WorkspaceId},
};

use super::values;

/// Moves the version of the workspace `id`, the one a proof names, for a change of its picture,
/// when it is one of `versions`, or whatever it is when there are none, and returns whether it
/// did (API-13, DATA-18).
pub(in crate::modules::workspaces) async fn touch(
    uow: &mut UnitOfWork,
    id: WorkspaceId,
    versions: Option<&[Version]>,
) -> Result<bool, Internal> {
    // The row's trigger moves its version and its update time (DATA-02).
    let versions = values(versions);
    let touched = sqlx::query!(
        "UPDATE workspaces SET version = version
        WHERE id = $1 AND ($2::bigint[] IS NULL OR version = ANY($2))",
        id.uuid(),
        versions.as_deref(),
    )
    .execute(uow.executor())
    .await?;
    Ok(touched.rows_affected() > 0)
}
