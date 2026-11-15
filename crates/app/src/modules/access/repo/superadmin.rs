//! The statements that `access` runs for a superadmin, whose proof names no workspace: the
//! census of the deployment's workspaces, which their list reads (WORKSPACES-029), and the lock
//! of the owner's membership, which their deletion of a workspace starts from (ACCESS-011).

use platform::{
    authz::Superadmin,
    db::UnitOfWork,
    errors::Internal,
    types::{UserId, WorkspaceId},
};

/// The owner and the number of members of each of `workspaces`, which a superadmin's list read,
/// for a superadmin, whose proof it takes, since it reads across workspaces (DATA-16). A
/// workspace without memberships, which a deletion took since, has no row.
pub(in crate::modules::access) async fn census(
    uow: &mut UnitOfWork,
    _superadmin: &Superadmin,
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

/// What [`hold_owner`] found of a workspace's owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::modules::access) enum Held {
    /// The owner's membership is locked until the unit of work ends.
    Locked,
    /// The workspace had an owner as the statement began, and has another now, since a transfer
    /// that the lock waited for made the member it waited at an admin: the next read locks the
    /// new owner's.
    Moved,
    /// The workspace has no owner, so it is gone, or never was (ACCESS-001).
    Gone,
}

/// Locks the membership of `workspace`'s owner `FOR UPDATE` until the unit of work ends, for a
/// superadmin's deletion of the workspace that the path names, whose proof it takes (DATA-16),
/// and tells whether it locked one (ACCESS-011). The statement reads whether the workspace has
/// an owner as it begins, before the lock's wait, and the lock skips a membership that is no
/// owner's once the wait ends: so it tells a workspace whose owner changed while it waited,
/// which still had one as it began, from one that has none.
pub(in crate::modules::access) async fn hold_owner(
    uow: &mut UnitOfWork,
    _superadmin: &Superadmin,
    workspace: WorkspaceId,
) -> Result<Held, Internal> {
    let row = sqlx::query!(
        r#"WITH held AS (
            SELECT id FROM workspace_memberships WHERE workspace_id = $1 AND role = 'owner'
            FOR UPDATE
        )
        SELECT EXISTS (SELECT 1 FROM held) AS "locked!",
            EXISTS (
                SELECT 1 FROM workspace_memberships WHERE workspace_id = $1 AND role = 'owner'
            ) AS "owned!""#,
        workspace.uuid(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(match (row.locked, row.owned) {
        (true, _) => Held::Locked,
        (false, true) => Held::Moved,
        (false, false) => Held::Gone,
    })
}
