//! The statement of the census of a deployment's workspaces, which a superadmin reads
//! (WORKSPACES-029).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{UserId, WorkspaceId},
};

/// The owner and the number of members of each of `workspaces`, which a superadmin's list read.
/// A workspace without memberships, which a deletion took since, has no row.
pub(in crate::modules::access) async fn census(
    uow: &mut UnitOfWork,
    workspaces: &[WorkspaceId],
) -> Result<Vec<(WorkspaceId, UserId, i64)>, Internal> {
    let workspaces: Vec<uuid::Uuid> = workspaces.iter().map(|id| id.uuid()).collect();
    let rows = sqlx::query!(
        r#"SELECT workspace_id,
            (array_agg(user_id) FILTER (WHERE role = 'owner'))[1] AS owner,
            count(*) AS "members!"
        FROM workspace_memberships
        WHERE workspace_id = ANY($1)
        GROUP BY workspace_id"#,
        &workspaces,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let owner = UserId::from_uuid(row.owner?);
            Some((WorkspaceId::from_uuid(row.workspace_id), owner, row.members))
        })
        .collect())
}
