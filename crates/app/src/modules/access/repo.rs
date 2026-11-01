//! The SQL of `workspace_memberships` (ARCH-10). Every query filters by the workspace or the user
//! the operation's proof names (DATA-22).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::Page,
    types::{Timestamp, UserId, WorkspaceId},
};

use super::model::{Membership, Role};

/// Adds `user` to `workspace` at `role`, and returns when they joined.
pub(super) async fn insert(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
    role: Role,
) -> Result<Timestamp, Internal> {
    let joined_at = sqlx::query_scalar!(
        "INSERT INTO workspace_memberships (workspace_id, user_id, role)
        VALUES ($1, $2, $3::text::workspace_role)
        RETURNING created_at",
        workspace.uuid(),
        user.uuid(),
        role.name(),
    )
    .fetch_one(uow.executor())
    .await?;
    Ok(Timestamp::from_database(joined_at))
}

/// The role of `user` in `workspace`, if they are a member, with their row locked `FOR SHARE`
/// until the unit of work ends, so a removal or a role change waits for it (PLATFORM-024).
pub(super) async fn role_held(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<Option<Role>, Internal> {
    let role = sqlx::query_scalar!(
        r#"SELECT role::text AS "role!" FROM workspace_memberships
        WHERE workspace_id = $1 AND user_id = $2
        FOR SHARE"#,
        workspace.uuid(),
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    role.as_deref().map(Role::from_name).transpose()
}

/// The role of `user` in `workspace`, if they are a member, with their row locked `FOR UPDATE`,
/// for an operation that changes it.
pub(super) async fn role_for_change(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<Option<Role>, Internal> {
    let role = sqlx::query_scalar!(
        r#"SELECT role::text AS "role!" FROM workspace_memberships
        WHERE workspace_id = $1 AND user_id = $2
        FOR UPDATE"#,
        workspace.uuid(),
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    role.as_deref().map(Role::from_name).transpose()
}

/// Deletes the membership of `user` in `workspace`.
pub(super) async fn delete(
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

/// A page of `workspace`'s members, the latest joined first, of `role` only and among `users`
/// only when given, read through the index on the workspace and the joining time (DATA-23).
pub(super) async fn members(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    role: Option<Role>,
    users: Option<&[UserId]>,
    page: &Page,
) -> Result<Vec<Membership>, Internal> {
    let users: Option<Vec<uuid::Uuid>> =
        users.map(|users| users.iter().map(|user| user.uuid()).collect());
    let rows = sqlx::query!(
        r#"SELECT workspace_id, user_id, role::text AS "role!", created_at
        FROM workspace_memberships
        WHERE workspace_id = $1
            AND ($2::text IS NULL OR role = $2::text::workspace_role)
            AND ($3::uuid[] IS NULL OR user_id = ANY($3))
            AND ($4::timestamptz IS NULL OR (created_at, user_id) < ($4, $5))
        ORDER BY created_at DESC, user_id DESC
        LIMIT $6"#,
        workspace.uuid(),
        role.map(Role::name),
        users.as_deref(),
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| membership(row.workspace_id, row.user_id, &row.role, row.created_at))
        .collect()
}

/// The user IDs of every member of `workspace`.
pub(super) async fn member_ids(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
) -> Result<Vec<UserId>, Internal> {
    let ids = sqlx::query_scalar!(
        "SELECT user_id FROM workspace_memberships WHERE workspace_id = $1",
        workspace.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(ids.into_iter().map(UserId::from_uuid).collect())
}

/// A page of `user`'s memberships, the latest joined first, among `workspaces` only when given,
/// read through the index on the user and the joining time (DATA-23).
pub(super) async fn joined(
    uow: &mut UnitOfWork,
    user: UserId,
    workspaces: Option<&[WorkspaceId]>,
    page: &Page,
) -> Result<Vec<Membership>, Internal> {
    let workspaces: Option<Vec<uuid::Uuid>> =
        workspaces.map(|workspaces| workspaces.iter().map(|id| id.uuid()).collect());
    let rows = sqlx::query!(
        r#"SELECT workspace_id, user_id, role::text AS "role!", created_at
        FROM workspace_memberships
        WHERE user_id = $1
            AND ($2::uuid[] IS NULL OR workspace_id = ANY($2))
            AND ($3::timestamptz IS NULL OR (created_at, workspace_id) < ($3, $4))
        ORDER BY created_at DESC, workspace_id DESC
        LIMIT $5"#,
        user.uuid(),
        workspaces.as_deref(),
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| membership(row.workspace_id, row.user_id, &row.role, row.created_at))
        .collect()
}

/// Every membership of `user`, the latest joined first.
pub(super) async fn memberships(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<Membership>, Internal> {
    let rows = sqlx::query!(
        r#"SELECT workspace_id, user_id, role::text AS "role!", created_at
        FROM workspace_memberships
        WHERE user_id = $1
        ORDER BY created_at DESC, workspace_id DESC"#,
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| membership(row.workspace_id, row.user_id, &row.role, row.created_at))
        .collect()
}

fn membership(
    workspace: uuid::Uuid,
    user: uuid::Uuid,
    role: &str,
    joined_at: time::OffsetDateTime,
) -> Result<Membership, Internal> {
    Ok(Membership {
        workspace_id: WorkspaceId::from_uuid(workspace),
        user_id: UserId::from_uuid(user),
        role: Role::from_name(role)?,
        joined_at: Timestamp::from_database(joined_at),
    })
}
