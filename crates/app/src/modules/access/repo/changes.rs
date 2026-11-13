//! The SQL that changes a membership's role or ends memberships (ARCH-10). Every statement
//! filters by the workspace or the user the operation's proof names (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{UserId, Version, WorkspaceId},
};

use super::membership;
use crate::modules::access::model::{Membership, Role};

/// Deletes the membership of `user` in `workspace` if their role is one of `roles`, and returns
/// whether it did. A row whose role isn't one of them is skipped without being locked, so a
/// removal never waits on a member it may not remove (DATA-20).
pub(in crate::modules::access) async fn delete_at(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
    roles: &[Role],
) -> Result<bool, Internal> {
    let roles: Vec<String> = roles.iter().map(|role| role.name().to_owned()).collect();
    let deleted = sqlx::query!(
        "DELETE FROM workspace_memberships
        WHERE workspace_id = $1 AND user_id = $2 AND role::text = ANY($3)",
        workspace.uuid(),
        user.uuid(),
        &roles,
    )
    .execute(uow.executor())
    .await?;
    Ok(deleted.rows_affected() > 0)
}

/// Deletes the membership of `user` in `workspace`.
pub(in crate::modules::access) async fn delete(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM workspace_memberships WHERE workspace_id = $1 AND user_id = $2",
        workspace.uuid(),
        user.uuid(),
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// Deletes every membership of `user`, and returns their workspaces.
pub(in crate::modules::access) async fn delete_all(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<WorkspaceId>, Internal> {
    let ids = sqlx::query_scalar!(
        "DELETE FROM workspace_memberships WHERE user_id = $1 RETURNING workspace_id",
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(ids.into_iter().map(WorkspaceId::from_uuid).collect())
}

/// Deletes every membership of `workspace` at `role`, and returns their users.
pub(in crate::modules::access) async fn delete_role(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    role: Role,
) -> Result<Vec<UserId>, Internal> {
    let ids = sqlx::query_scalar!(
        "DELETE FROM workspace_memberships
        WHERE workspace_id = $1 AND role = $2::text::workspace_role
        RETURNING user_id",
        workspace.uuid(),
        role.name(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(ids.into_iter().map(UserId::from_uuid).collect())
}

/// Locks the membership of `user` in `workspace` `FOR UPDATE` until the unit of work ends, if
/// their role is one of `roles`, and returns it with its row's version. A row whose role isn't
/// one of them is skipped without being locked, so a change never waits on a member it may not
/// change (DATA-20).
pub(in crate::modules::access) async fn hold_at(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
    roles: &[Role],
) -> Result<Option<(Membership, Version)>, Internal> {
    let roles: Vec<String> = roles.iter().map(|role| role.name().to_owned()).collect();
    let row = sqlx::query!(
        r#"SELECT id, workspace_id, user_id, role::text AS "role!", created_at, version
        FROM workspace_memberships
        WHERE workspace_id = $1 AND user_id = $2 AND role::text = ANY($3)
        FOR UPDATE"#,
        workspace.uuid(),
        user.uuid(),
        &roles,
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| {
        Ok((
            membership(
                row.id,
                row.workspace_id,
                row.user_id,
                &row.role,
                row.created_at,
            )?,
            Version::from_database(row.version),
        ))
    })
    .transpose()
}

/// Makes `role` the role of `user` in `workspace`, if the row's version is one of `versions`, or
/// whatever its version when `versions` is `None` (DATA-18). Returns the row's new version, or
/// `None` when it changed nothing.
pub(in crate::modules::access) async fn set_role(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
    role: Role,
    versions: Option<&[Version]>,
) -> Result<Option<Version>, Internal> {
    let versions: Option<Vec<i64>> =
        versions.map(|versions| versions.iter().map(|version| version.value()).collect());
    let version = sqlx::query_scalar!(
        "UPDATE workspace_memberships SET role = $3::text::workspace_role
        WHERE workspace_id = $1 AND user_id = $2 AND ($4::bigint[] IS NULL OR version = ANY($4))
        RETURNING version",
        workspace.uuid(),
        user.uuid(),
        role.name(),
        versions.as_deref(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(version.map(Version::from_database))
}
