//! The SQL of the operator's setup of a workspace: the row that keeps the name its run was given
//! (the workspaces spec's "The setup subcommand").

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{Timestamp, WorkspaceId},
};

use crate::modules::workspaces::model::{ExportedSetup, WorkspaceName};

/// Records that a run of the setup, which was given `name`, created the workspace `id`, the one
/// its unit of work just created, and returns whether it did: `false` when an earlier run was
/// given the name, which the unique constraint `workspace_setups_name_key` refuses, without
/// aborting the unit of work (WORKSPACES-027, ERR-05).
pub(in crate::modules::workspaces) async fn record(
    uow: &mut UnitOfWork,
    id: WorkspaceId,
    name: &WorkspaceName,
) -> Result<bool, Internal> {
    let recorded = sqlx::query!(
        "INSERT INTO workspace_setups (workspace_id, name) VALUES ($1, $2)
        ON CONFLICT ON CONSTRAINT workspace_setups_name_key DO NOTHING",
        id.uuid(),
        name.as_str(),
    )
    .execute(uow.executor())
    .await?;
    Ok(recorded.rows_affected() > 0)
}

/// The setups of the workspaces `ids` names, which came from a query filtered by tenant, as an
/// export holds them (DATA-16).
pub(in crate::modules::workspaces) async fn exported(
    uow: &mut UnitOfWork,
    ids: &[WorkspaceId],
) -> Result<Vec<ExportedSetup>, Internal> {
    let ids: Vec<uuid::Uuid> = ids.iter().map(|id| id.uuid()).collect();
    let rows = sqlx::query!(
        "SELECT workspace_id, name, created_at FROM workspace_setups
        WHERE workspace_id = ANY($1) ORDER BY workspace_id",
        &ids,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ExportedSetup {
            workspace_id: WorkspaceId::from_uuid(row.workspace_id),
            name: WorkspaceName::from_stored(row.name),
            created_at: Timestamp::from_database(row.created_at),
        })
        .collect())
}
