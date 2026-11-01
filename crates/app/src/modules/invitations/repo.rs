//! The SQL of `invitations` (ARCH-10), but the expiry sweep's, which `expiry.rs` holds. A
//! workspace's invitations are filtered by the workspace from the proof, and an invitee's by the
//! caller's address (DATA-22).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::Page,
    types::{EmailAddress, Timestamp, UserId, WorkspaceId},
};

use super::model::{Found, Invitation, Pending};
use crate::modules::access::Role;

/// Inserts the invitation of `email` to `workspace` at `role`, or refreshes the one there is: its
/// role, inviter and expiry are replaced and its ID kept. Returns it, with whether it is new
/// (INVITATIONS-001, INVITATIONS-003).
pub(super) async fn upsert(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    email: &EmailAddress,
    role: Role,
    inviter_id: UserId,
    expiry_days: u32,
) -> Result<(Invitation, bool), Internal> {
    let days = i32::try_from(expiry_days).map_err(Internal::new)?;
    let row = sqlx::query!(
        r#"INSERT INTO invitations (workspace_id, email, role, inviter_id, expires_at)
        VALUES ($1, $2, $3::text::workspace_role, $4, now() + make_interval(days => $5))
        ON CONFLICT ON CONSTRAINT invitations_workspace_id_email_key DO UPDATE SET
            role = EXCLUDED.role,
            inviter_id = EXCLUDED.inviter_id,
            expires_at = EXCLUDED.expires_at
        RETURNING id, created_at, expires_at, (xmax = 0) AS "created!""#,
        workspace.uuid(),
        email.as_str(),
        role.name(),
        inviter_id.uuid(),
        days,
    )
    .fetch_one(uow.executor())
    .await?;
    let invitation = Invitation {
        id: row.id,
        workspace_id: workspace,
        email: email.clone(),
        role,
        inviter_id,
        created_at: Timestamp::from_database(row.created_at),
        expires_at: Timestamp::from_database(row.expires_at),
    };
    Ok((invitation, row.created))
}

/// A page of the unexpired invitations to `email`, the latest first, to `workspaces` only when a
/// search named them (INVITATIONS-004).
pub(super) async fn pending(
    uow: &mut UnitOfWork,
    email: &EmailAddress,
    workspaces: Option<&[WorkspaceId]>,
    page: &Page,
) -> Result<Vec<Pending>, Internal> {
    let workspaces: Option<Vec<uuid::Uuid>> =
        workspaces.map(|workspaces| workspaces.iter().map(|id| id.uuid()).collect());
    let rows = sqlx::query!(
        r#"SELECT id, workspace_id, inviter_id, role::text AS "role!", created_at, expires_at
        FROM invitations
        WHERE email = $1 AND expires_at > now()
            AND ($2::uuid[] IS NULL OR workspace_id = ANY($2))
            AND ($3::timestamptz IS NULL OR (created_at, id) < ($3, $4))
        ORDER BY created_at DESC, id DESC
        LIMIT $5"#,
        email.as_str(),
        workspaces.as_deref(),
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Pending {
                id: row.id,
                workspace_id: WorkspaceId::from_uuid(row.workspace_id),
                inviter_id: UserId::from_uuid(row.inviter_id),
                role: Role::from_name(&row.role)?,
                created_at: Timestamp::from_database(row.created_at),
                expires_at: Timestamp::from_database(row.expires_at),
            })
        })
        .collect()
}

/// The workspaces of every unexpired invitation to `email`, which a search hands `workspaces`.
pub(super) async fn workspace_ids(
    uow: &mut UnitOfWork,
    email: &EmailAddress,
) -> Result<Vec<WorkspaceId>, Internal> {
    let ids = sqlx::query_scalar!(
        "SELECT workspace_id FROM invitations WHERE email = $1 AND expires_at > now()",
        email.as_str(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(ids.into_iter().map(WorkspaceId::from_uuid).collect())
}

/// The invitation `id`, if it is addressed to `email`, locked until the unit of work ends, so two
/// acceptances of it take turns (DATA-25), with whether it expired, by the database's clock.
pub(super) async fn find(
    uow: &mut UnitOfWork,
    id: uuid::Uuid,
    email: &EmailAddress,
) -> Result<Option<Found>, Internal> {
    let row = sqlx::query!(
        r#"SELECT workspace_id, role::text AS "role!", expires_at <= now() AS "expired!"
        FROM invitations
        WHERE id = $1 AND email = $2
        FOR UPDATE"#,
        id,
        email.as_str(),
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| {
        Ok(Found {
            workspace_id: WorkspaceId::from_uuid(row.workspace_id),
            role: Role::from_name(&row.role)?,
            expired: row.expired,
        })
    })
    .transpose()
}

/// Deletes the invitation `id` addressed to `email`.
pub(super) async fn delete(
    uow: &mut UnitOfWork,
    id: uuid::Uuid,
    email: &EmailAddress,
) -> Result<(), Internal> {
    sqlx::query!(
        "DELETE FROM invitations WHERE id = $1 AND email = $2",
        id,
        email.as_str(),
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}
