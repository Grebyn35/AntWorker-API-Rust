//! The SQL of `workspaces` (ARCH-10). A workspace is read only by the ID of the workspace a proof
//! names, or by IDs that came from a query filtered by tenant (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::Search,
    types::{Timestamp, Version, WorkspaceId},
};

use super::model::{ExportedWorkspace, WorkspaceName};

/// Inserts a workspace named `name`, and returns its ID, when it was created and its version.
pub(super) async fn insert(
    uow: &mut UnitOfWork,
    name: &WorkspaceName,
) -> Result<(WorkspaceId, Timestamp, Version), Internal> {
    let row = sqlx::query!(
        "INSERT INTO workspaces (name) VALUES ($1) RETURNING id, created_at, version",
        name.as_str(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok((
        WorkspaceId::from_uuid(row.id),
        Timestamp::from_database(row.created_at),
        Version::from_database(row.version),
    ))
}

/// Deletes the workspace `id`, the one a proof names, or one whose only member is the user whose
/// account the unit of work deletes, and everything in it, through the foreign keys that name it
/// (WORKSPACES-017, ACCOUNTS-013).
pub(super) async fn delete(uow: &mut UnitOfWork, id: WorkspaceId) -> Result<(), Internal> {
    sqlx::query!("DELETE FROM workspaces WHERE id = $1", id.uuid())
        .execute(uow.executor())
        .await?;
    Ok(())
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
/// `versions`, or whatever its version when `versions` is `None`, and returns whether it did
/// (DATA-18).
pub(super) async fn rename(
    uow: &mut UnitOfWork,
    id: WorkspaceId,
    name: &WorkspaceName,
    versions: Option<&[Version]>,
) -> Result<bool, Internal> {
    let versions = values(versions);
    let renamed = sqlx::query!(
        "UPDATE workspaces SET name = $2 \
         WHERE id = $1 AND ($3::bigint[] IS NULL OR version = ANY($3))",
        id.uuid(),
        name.as_str(),
        versions.as_deref(),
    )
    .execute(uow.executor())
    .await?;
    Ok(renamed.rows_affected() > 0)
}

/// The values of `versions`, as a query compares them with a row's.
fn values(versions: Option<&[Version]>) -> Option<Vec<i64>> {
    versions.map(|versions| versions.iter().map(|version| version.value()).collect())
}

// part: files
/// Moves the version of the workspace `id`, the one a proof names, for a change of its picture,
/// when it is one of `versions`, or whatever it is when there are none, and returns whether it
/// did (API-13, DATA-18, the workspaces spec's "The picture").
pub(super) async fn touch(
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
// end part: files

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

/// The workspaces `ids`, which came from the user's own memberships, as their export holds them,
/// in the order of their IDs (ACCOUNTS-024, DATA-16).
pub(super) async fn exported(
    uow: &mut UnitOfWork,
    ids: &[WorkspaceId],
) -> Result<Vec<ExportedWorkspace>, Internal> {
    let ids: Vec<uuid::Uuid> = ids.iter().map(|id| id.uuid()).collect();
    let rows = sqlx::query!(
        "SELECT id, name, created_at, updated_at FROM workspaces WHERE id = ANY($1) ORDER BY id",
        &ids,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| ExportedWorkspace {
            id: WorkspaceId::from_uuid(row.id),
            name: WorkspaceName::from_stored(row.name),
            created_at: Timestamp::from_database(row.created_at),
            updated_at: Timestamp::from_database(row.updated_at),
        })
        .collect())
}
