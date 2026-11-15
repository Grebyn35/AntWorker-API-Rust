//! The statements of a superadmin's routes, which read across workspaces (the workspaces spec's
//! "A superadmin's routes").

use platform::{
    authz::Superadmin,
    db::UnitOfWork,
    errors::Internal,
    lists::{Page, Search},
    types::{Timestamp, WorkspaceId},
};

use crate::modules::workspaces::model::WorkspaceName;

/// A page of every workspace, with its name and when it was created, the latest created first,
/// kept to those whose name contains `search`, ignoring case in every script (PLATFORM-047),
/// read through the index on their creation time and ID (DATA-17), for a superadmin, whose proof
/// it takes, since it reads across workspaces (DATA-16).
pub(in crate::modules::workspaces) async fn every(
    uow: &mut UnitOfWork,
    _superadmin: &Superadmin,
    search: Option<&Search>,
    page: &Page,
) -> Result<Vec<(WorkspaceId, WorkspaceName, Timestamp)>, Internal> {
    let rows = sqlx::query!(
        "SELECT id, name, created_at
        FROM workspaces
        WHERE ($1::text IS NULL OR name COLLATE pg_unicode_fast ILIKE $1)
            AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4",
        search.map(Search::pattern),
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                WorkspaceId::from_uuid(row.id),
                WorkspaceName::from_stored(row.name),
                Timestamp::from_database(row.created_at),
            )
        })
        .collect())
}
