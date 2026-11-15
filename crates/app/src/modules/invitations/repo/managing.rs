//! The SQL of what a workspace's members do with its invitations: reading and listing them, and
//! changing, sending again and cancelling one (ARCH-10). Every statement filters by the workspace
//! from the proof (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::{Page, Search},
    types::{EmailAddress, Timestamp, UserId, Version, WorkspaceId},
};

use crate::modules::{
    access::Role,
    invitations::model::{CurrentInvitation, Invitation},
};

/// An invitation's row, as the statements below return it.
struct Row {
    id: uuid::Uuid,
    email: String,
    role: String,
    inviter_id: Option<uuid::Uuid>,
    created_at: time::OffsetDateTime,
    expires_at: time::OffsetDateTime,
    version: i64,
}

impl Row {
    /// The invitation of `workspace` that the row holds, with its version.
    fn current(self, workspace: WorkspaceId) -> Result<CurrentInvitation, Internal> {
        Ok(CurrentInvitation {
            invitation: Invitation {
                id: self.id,
                workspace_id: workspace,
                email: EmailAddress::from_stored(self.email),
                role: Role::from_name(&self.role)?,
                inviter_id: self.inviter_id.map(UserId::from_uuid),
                created_at: Timestamp::from_database(self.created_at),
                expires_at: Timestamp::from_database(self.expires_at),
            },
            version: Version::from_database(self.version),
        })
    }
}

/// The invitation `id` of `workspace`, expired or not, with its row's version (INVITATIONS-011).
pub(in crate::modules::invitations) async fn read(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    id: uuid::Uuid,
) -> Result<Option<CurrentInvitation>, Internal> {
    let row = sqlx::query_as!(
        Row,
        r#"SELECT id, email, role::text AS "role!", inviter_id, created_at, expires_at, version
        FROM invitations
        WHERE id = $1 AND workspace_id = $2"#,
        id,
        workspace.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| row.current(workspace)).transpose()
}

/// A page of the invitations of `workspace`, expired or not, the latest first, of those whose
/// email contains `search` only, when there is one (INVITATIONS-012, PLATFORM-047).
pub(in crate::modules::invitations) async fn page(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    search: Option<&Search>,
    page: &Page,
) -> Result<Vec<Invitation>, Internal> {
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT id, email, role::text AS "role!", inviter_id, created_at, expires_at, version
        FROM invitations
        WHERE workspace_id = $1
            AND ($2::text IS NULL OR email COLLATE pg_unicode_fast ILIKE $2)
            AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
        ORDER BY created_at DESC, id DESC
        LIMIT $5"#,
        workspace.uuid(),
        search.map(Search::pattern),
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| Ok(row.current(workspace)?.invitation))
        .collect()
}

/// The invitation `id` of `workspace`, expired or not, with its row's version, locked `FOR UPDATE`
/// until the unit of work ends, so the operations on it take turns (DATA-20).
pub(in crate::modules::invitations) async fn hold(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    id: uuid::Uuid,
) -> Result<Option<CurrentInvitation>, Internal> {
    let row = sqlx::query_as!(
        Row,
        r#"SELECT id, email, role::text AS "role!", inviter_id, created_at, expires_at, version
        FROM invitations
        WHERE id = $1 AND workspace_id = $2
        FOR UPDATE"#,
        id,
        workspace.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| row.current(workspace)).transpose()
}

/// Makes `role` the role of the invitation `id` of `workspace`, if the row's version is one of
/// `versions`, or whatever its version when `versions` is `None` (DATA-18). Returns it as it then
/// stands, or `None` when it changed nothing (INVITATIONS-013).
pub(in crate::modules::invitations) async fn set_role(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    id: uuid::Uuid,
    role: Role,
    versions: Option<&[Version]>,
) -> Result<Option<CurrentInvitation>, Internal> {
    let versions: Option<Vec<i64>> =
        versions.map(|versions| versions.iter().map(|version| version.value()).collect());
    let row = sqlx::query_as!(
        Row,
        r#"UPDATE invitations SET role = $3::text::workspace_role
        WHERE id = $1 AND workspace_id = $2 AND ($4::bigint[] IS NULL OR version = ANY($4))
        RETURNING id, email, role::text AS "role!", inviter_id, created_at, expires_at, version"#,
        id,
        workspace.uuid(),
        role.name(),
        versions.as_deref(),
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| row.current(workspace)).transpose()
}

/// Makes `inviter` the inviter of the invitation `id` of `workspace`, and makes it expire
/// `expiry_days` after the database's clock, as sending it again does, and returns it as it then
/// stands (INVITATIONS-015, DATA-03).
pub(in crate::modules::invitations) async fn renew(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    id: uuid::Uuid,
    inviter: UserId,
    expiry_days: u32,
) -> Result<CurrentInvitation, Internal> {
    let days = i32::try_from(expiry_days).map_err(Internal::new)?;
    let row = sqlx::query_as!(
        Row,
        r#"UPDATE invitations
        SET inviter_id = $3, expires_at = now() + make_interval(days => $4)
        WHERE id = $1 AND workspace_id = $2
        RETURNING id, email, role::text AS "role!", inviter_id, created_at, expires_at, version"#,
        id,
        workspace.uuid(),
        inviter.uuid(),
        days,
    )
    .fetch_one(uow.executor())
    .await?;
    row.current(workspace)
}

/// Deletes the invitation `id` of `workspace` (INVITATIONS-016).
pub(in crate::modules::invitations) async fn cancel(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    id: uuid::Uuid,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM invitations WHERE id = $1 AND workspace_id = $2",
        id,
        workspace.uuid(),
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}
