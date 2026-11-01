//! The SQL of `workspaces` (ARCH-10). A workspace is read only by IDs that came from a query
//! filtered by tenant (DATA-22).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::Search,
    types::{Timestamp, WorkspaceId},
};

use super::model::WorkspaceName;

/// Inserts a workspace named `name`, and returns its ID and when it was created.
pub(super) async fn insert(
    uow: &mut UnitOfWork,
    name: &WorkspaceName,
) -> Result<(WorkspaceId, Timestamp), Internal> {
    let row = sqlx::query!(
        "INSERT INTO workspaces (name) VALUES ($1) RETURNING id, created_at",
        name.as_str(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok((
        WorkspaceId::from_uuid(row.id),
        Timestamp::from_database(row.created_at),
    ))
}

/// The names of the workspaces `ids` names, which came from a query filtered by tenant.
pub(super) async fn names(
    uow: &mut UnitOfWork,
    ids: &[WorkspaceId],
) -> Result<Vec<(WorkspaceId, WorkspaceName)>, Internal> {
    let ids: Vec<uuid::Uuid> = ids.iter().map(|id| id.uuid()).collect();
    let rows = sqlx::query!("SELECT id, name FROM workspaces WHERE id = ANY($1)", &ids)
        .fetch_all(uow.executor())
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                WorkspaceId::from_uuid(row.id),
                WorkspaceName::from_stored(row.name),
            )
        })
        .collect())
}

/// The IDs among `ids`, which came from a query filtered by tenant, of the workspaces whose name
/// contains `search`, ignoring case in every script (PLATFORM-060).
pub(super) async fn matching(
    uow: &mut UnitOfWork,
    ids: &[WorkspaceId],
    search: &Search,
) -> Result<Vec<WorkspaceId>, Internal> {
    let ids: Vec<uuid::Uuid> = ids.iter().map(|id| id.uuid()).collect();
    let matched = sqlx::query_scalar!(
        "SELECT id FROM workspaces WHERE id = ANY($1) AND name COLLATE pg_unicode_fast ILIKE $2",
        &ids,
        search.pattern(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(matched.into_iter().map(WorkspaceId::from_uuid).collect())
}
