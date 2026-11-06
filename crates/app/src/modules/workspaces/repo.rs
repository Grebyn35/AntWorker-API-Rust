//! The SQL of `workspaces` (ARCH-10). A workspace is read only by the ID of the workspace a proof
//! names, or by IDs that came from a query filtered by tenant (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::Search,
    types::{Timestamp, Version, WorkspaceId},
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

/// The name, the creation time and the version of the workspace `id`, the one a proof names.
pub(super) async fn read(
    uow: &mut UnitOfWork,
    id: WorkspaceId,
) -> Result<(WorkspaceName, Timestamp, Version), Internal> {
    let row = sqlx::query!(
        "SELECT name, created_at, version FROM workspaces WHERE id = $1",
        id.uuid(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok((
        WorkspaceName::from_stored(row.name),
        Timestamp::from_database(row.created_at),
        Version::from_database(row.version),
    ))
}

/// Renames the workspace `id`, the one a proof names, to `name`, if its version is one of
/// `versions`, or whatever its version when `versions` is `None` (DATA-18). Returns its creation
/// time and its new version, or `None` when its version isn't one of `versions`.
pub(super) async fn rename(
    uow: &mut UnitOfWork,
    id: WorkspaceId,
    name: &WorkspaceName,
    versions: Option<&[Version]>,
) -> Result<Option<(Timestamp, Version)>, Internal> {
    let versions: Option<Vec<i64>> =
        versions.map(|versions| versions.iter().map(|version| version.value()).collect());
    let row = sqlx::query!(
        "UPDATE workspaces SET name = $2 \
         WHERE id = $1 AND ($3::bigint[] IS NULL OR version = ANY($3)) \
         RETURNING created_at, version",
        id.uuid(),
        name.as_str(),
        versions.as_deref(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(row.map(|row| {
        (
            Timestamp::from_database(row.created_at),
            Version::from_database(row.version),
        )
    }))
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
/// contains `search`, ignoring case in every script (PLATFORM-047).
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
