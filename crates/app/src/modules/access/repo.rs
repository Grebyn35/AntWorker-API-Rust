//! The SQL of `workspace_memberships` (ARCH-10). Every query filters by the workspace or the user
//! the operation's proof names (DATA-16).

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

// part: invitations
/// Locks the membership of `workspace`'s owner `FOR KEY SHARE` until the unit of work ends, and
/// returns whether there is one (ACCESS-001, ACCESS-008).
pub(super) async fn hold_owner(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
) -> Result<bool, Internal> {
    let owner = sqlx::query_scalar!(
        "SELECT id FROM workspace_memberships WHERE workspace_id = $1 AND role = 'owner'
        FOR KEY SHARE",
        workspace.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(owner.is_some())
}

/// Adds `user` to `workspace` at `role`, and returns when they joined, or `None` when the unique
/// constraint on the pair finds them a member already (ERR-05).
pub(super) async fn insert_unless_member(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
    role: Role,
) -> Result<Option<Timestamp>, Internal> {
    let joined_at = sqlx::query_scalar!(
        "INSERT INTO workspace_memberships (workspace_id, user_id, role)
        VALUES ($1, $2, $3::text::workspace_role)
        ON CONFLICT ON CONSTRAINT workspace_memberships_workspace_id_user_id_key DO NOTHING
        RETURNING created_at",
        workspace.uuid(),
        user.uuid(),
        role.name(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(joined_at.map(Timestamp::from_database))
}
// end part: invitations

/// The role of `user` in `workspace`, if they are a member, with their row locked `FOR SHARE`
/// until the unit of work ends, so a removal or a role change waits for it (PLATFORM-009).
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

/// The role of `user` in `workspace`, if they are a member, read without a lock.
pub(super) async fn role(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    user: UserId,
) -> Result<Option<Role>, Internal> {
    let role = sqlx::query_scalar!(
        r#"SELECT role::text AS "role!" FROM workspace_memberships
        WHERE workspace_id = $1 AND user_id = $2"#,
        workspace.uuid(),
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    role.as_deref().map(Role::from_name).transpose()
}

/// Deletes the membership of `user` in `workspace` if their role is one of `roles`, and returns
/// whether it did. A row whose role isn't one of them is skipped without being locked, so a
/// removal never waits on a member it may not remove (DATA-20).
pub(super) async fn delete_at(
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

/// A page of `workspace`'s members, the latest joined first, of `role` only and among `users`
/// only when given, read through the index on the workspace and the joining time (DATA-17).
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
        r#"SELECT id, workspace_id, user_id, role::text AS "role!", created_at
        FROM workspace_memberships
        WHERE workspace_id = $1
            AND ($2::text IS NULL OR role = $2::text::workspace_role)
            AND ($3::uuid[] IS NULL OR user_id = ANY($3))
            AND ($4::timestamptz IS NULL OR (created_at, id) < ($4, $5))
        ORDER BY created_at DESC, id DESC
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
        .map(|row| {
            membership(
                row.id,
                row.workspace_id,
                row.user_id,
                &row.role,
                row.created_at,
            )
        })
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
/// read through the index on the user and the joining time (DATA-17).
pub(super) async fn joined(
    uow: &mut UnitOfWork,
    user: UserId,
    workspaces: Option<&[WorkspaceId]>,
    page: &Page,
) -> Result<Vec<Membership>, Internal> {
    let workspaces: Option<Vec<uuid::Uuid>> =
        workspaces.map(|workspaces| workspaces.iter().map(|id| id.uuid()).collect());
    let rows = sqlx::query!(
        r#"SELECT id, workspace_id, user_id, role::text AS "role!", created_at
        FROM workspace_memberships
        WHERE user_id = $1
            AND ($2::uuid[] IS NULL OR workspace_id = ANY($2))
            AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
        ORDER BY created_at DESC, id DESC
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
        .map(|row| {
            membership(
                row.id,
                row.workspace_id,
                row.user_id,
                &row.role,
                row.created_at,
            )
        })
        .collect()
}

/// The IDs of every workspace `user` belongs to.
pub(super) async fn workspace_ids(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<WorkspaceId>, Internal> {
    let ids = sqlx::query_scalar!(
        "SELECT workspace_id FROM workspace_memberships WHERE user_id = $1",
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(ids.into_iter().map(WorkspaceId::from_uuid).collect())
}

/// Every membership of `user`, with its workspace and role, each locked `FOR UPDATE` until the
/// unit of work ends, in the order of the workspaces' IDs (ACCESS-008).
pub(super) async fn held(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Vec<(WorkspaceId, Role)>, Internal> {
    let rows = sqlx::query!(
        r#"SELECT workspace_id, role::text AS "role!" FROM workspace_memberships
        WHERE user_id = $1
        ORDER BY workspace_id
        FOR UPDATE"#,
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                WorkspaceId::from_uuid(row.workspace_id),
                Role::from_name(&row.role)?,
            ))
        })
        .collect()
}

/// Whether any of `workspaces`, which came from the user's own memberships, has a member other
/// than `user` (ACCESS-008).
pub(super) async fn others_in(
    uow: &mut UnitOfWork,
    workspaces: &[WorkspaceId],
    user: UserId,
) -> Result<bool, Internal> {
    let workspaces: Vec<uuid::Uuid> = workspaces.iter().map(|id| id.uuid()).collect();
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
            SELECT 1 FROM workspace_memberships WHERE workspace_id = ANY($1) AND user_id <> $2
        ) AS "exists!""#,
        &workspaces,
        user.uuid(),
    )
    .fetch_one(uow.executor())
    .await?)
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

/// Deletes every membership of `user`, and returns their workspaces.
pub(super) async fn delete_all(
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

/// Every membership of each of `users`, with its workspace, role and the transaction that added
/// it, for the hub's streams: the IDs come from the hub's own streams' principals (DATA-16).
pub(super) async fn memberships_of(
    uow: &mut UnitOfWork,
    users: &[UserId],
) -> Result<Vec<(UserId, WorkspaceId, Role, u64)>, Internal> {
    let users: Vec<uuid::Uuid> = users.iter().map(|user| user.uuid()).collect();
    let rows = sqlx::query!(
        r#"SELECT user_id, workspace_id, role::text AS "role!", joined_xact::text AS "joined!"
        FROM workspace_memberships
        WHERE user_id = ANY($1)"#,
        &users,
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            let joined = row.joined.parse().map_err(Internal::new)?;
            Ok((
                UserId::from_uuid(row.user_id),
                WorkspaceId::from_uuid(row.workspace_id),
                Role::from_name(&row.role)?,
                joined,
            ))
        })
        .collect()
}

fn membership(
    id: uuid::Uuid,
    workspace: uuid::Uuid,
    user: uuid::Uuid,
    role: &str,
    joined_at: time::OffsetDateTime,
) -> Result<Membership, Internal> {
    Ok(Membership {
        id,
        workspace_id: WorkspaceId::from_uuid(workspace),
        user_id: UserId::from_uuid(user),
        role: Role::from_name(role)?,
        joined_at: Timestamp::from_database(joined_at),
    })
}
