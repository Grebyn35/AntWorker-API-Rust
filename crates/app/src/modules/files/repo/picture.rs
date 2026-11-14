//! The SQL of workspaces' pictures. Every statement filters by the workspaces it is given: the
//! one a proof names, whose row `workspaces` has locked, or workspaces that came from a query
//! filtered by tenant (DATA-16).

use platform::{db::UnitOfWork, errors::Internal, storage::ObjectKey, types::WorkspaceId};

/// Inserts the picture of `workspace`, and returns its ID.
pub(in crate::modules::files) async fn insert(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    media_type: &str,
    size_bytes: u64,
    crc32: u32,
    object_key: &ObjectKey,
) -> Result<uuid::Uuid, Internal> {
    let size_bytes = i64::try_from(size_bytes).map_err(Internal::new)?;
    Ok(sqlx::query_scalar!(
        "INSERT INTO workspace_pictures (workspace_id, media_type, size_bytes, crc32, object_key)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id",
        workspace.uuid(),
        media_type,
        size_bytes,
        i64::from(crc32),
        object_key.as_str(),
    )
    .fetch_one(uow.executor())
    .await?)
}

/// Deletes the picture of `workspace`, and returns its ID, if it had one.
pub(in crate::modules::files) async fn delete(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
) -> Result<Option<uuid::Uuid>, Internal> {
    Ok(sqlx::query_scalar!(
        "DELETE FROM workspace_pictures WHERE workspace_id = $1 RETURNING id",
        workspace.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?)
}

/// The ID of the picture of each workspace among `workspaces` that has one.
pub(in crate::modules::files) async fn ids(
    uow: &mut UnitOfWork,
    workspaces: &[WorkspaceId],
) -> Result<Vec<(WorkspaceId, uuid::Uuid)>, Internal> {
    let workspaces: Vec<uuid::Uuid> = workspaces
        .iter()
        .map(|workspace| workspace.uuid())
        .collect();
    let rows = sqlx::query!(
        "SELECT workspace_id, id FROM workspace_pictures WHERE workspace_id = ANY($1)",
        &workspaces,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (WorkspaceId::from_uuid(row.workspace_id), row.id))
        .collect())
}
