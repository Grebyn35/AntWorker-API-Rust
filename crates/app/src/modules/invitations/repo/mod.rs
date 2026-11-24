//! The SQL of `invitations` (ARCH-10), but the expiry sweep's, which `jobs.rs` holds: inviting,
//! an invitee's own invitations, accepting, declining and the export here, and what a workspace's
//! members do with its invitations in `managing`. A workspace's invitations are filtered by the
//! workspace from the proof, or, in the export, by one whose only member is the user, which came
//! from their own memberships, and an invitee's by the caller's address (DATA-16).

pub(super) mod managing;

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::Page,
    personal_data::PAGE,
    types::{EmailAddress, Timestamp, UserId, Version, WorkspaceId},
};

use super::model::{CurrentInvitation, ExportedInvitation, Found, Invitation, Pending};
use crate::modules::access::Role;

/// Inserts the invitation of `email` to `workspace` at `role`, or refreshes the one there is if
/// it offers one of `offerable`, and, unless `versions` is `None`, if its row's version is one of
/// them (DATA-18): its role, inviter and expiry are replaced and its ID kept. Returns it, with its
/// row's version and whether it is new, or `None` for an invitation there is at another role or
/// version, which it leaves as it was, but locked (INVITATIONS-001, INVITATIONS-003,
/// INVITATIONS-014, INVITATIONS-018).
pub(super) async fn upsert(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    (email, role): (&EmailAddress, Role),
    inviter_id: UserId,
    expiry_days: u32,
    (offerable, versions): (&[Role], Option<&[Version]>),
) -> Result<Option<(CurrentInvitation, bool)>, Internal> {
    let days = i32::try_from(expiry_days).map_err(Internal::new)?;
    let offerable: Vec<String> = offerable
        .iter()
        .map(|role| role.name().to_owned())
        .collect();
    let versions: Option<Vec<i64>> =
        versions.map(|versions| versions.iter().map(|version| version.value()).collect());
    let row = sqlx::query!(
        r#"INSERT INTO invitations (workspace_id, email, role, inviter_id, expires_at)
        VALUES ($1, $2, $3::text::workspace_role, $4, now() + make_interval(days => $5))
        ON CONFLICT ON CONSTRAINT invitations_workspace_id_email_key DO UPDATE SET
            role = EXCLUDED.role,
            inviter_id = EXCLUDED.inviter_id,
            expires_at = EXCLUDED.expires_at
        WHERE invitations.role::text = ANY($6)
            AND ($7::bigint[] IS NULL OR invitations.version = ANY($7))
        RETURNING id, created_at, expires_at, version, (xmax = 0) AS "created!""#,
        workspace.uuid(),
        email.as_str(),
        role.name(),
        inviter_id.uuid(),
        days,
        &offerable,
        versions.as_deref(),
    )
    .fetch_optional(uow.executor())
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let invitation = Invitation {
        id: row.id,
        workspace_id: workspace,
        email: email.clone(),
        role,
        inviter_id: Some(inviter_id),
        created_at: Timestamp::from_database(row.created_at),
        expires_at: Timestamp::from_database(row.expires_at),
    };
    let current = CurrentInvitation {
        invitation,
        version: Version::from_database(row.version),
    };
    Ok(Some((current, row.created)))
}

/// The role that the invitation of `email` to `workspace` offers, which the unit of work holds
/// once `upsert` has left it as it was (INVITATIONS-018).
pub(super) async fn offered(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    email: &EmailAddress,
) -> Result<Role, Internal> {
    let role = sqlx::query_scalar!(
        r#"SELECT role::text AS "role!" FROM invitations WHERE workspace_id = $1 AND email = $2"#,
        workspace.uuid(),
        email.as_str(),
    )
    .fetch_one(uow.executor())
    .await?;
    Role::from_name(&role)
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
                inviter_id: row.inviter_id.map(UserId::from_uuid),
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

/// The workspace of the invitation `id`, if it is addressed to `email`, read without a lock, so an
/// acceptance or a decline learns whose owner's membership to lock before it locks the invitation
/// (DATA-20).
pub(super) async fn workspace_of(
    uow: &mut UnitOfWork,
    id: uuid::Uuid,
    email: &EmailAddress,
) -> Result<Option<WorkspaceId>, Internal> {
    let workspace = sqlx::query_scalar!(
        "SELECT workspace_id FROM invitations WHERE id = $1 AND email = $2",
        id,
        email.as_str(),
    )
    .fetch_optional(uow.executor())
    .await?;
    Ok(workspace.map(WorkspaceId::from_uuid))
}

/// The invitation `id`, if it is addressed to `email`, locked until the unit of work ends, so two
/// acceptances of it take turns (DATA-20), with whether it expired, by the database's clock.
pub(super) async fn find(
    uow: &mut UnitOfWork,
    id: uuid::Uuid,
    email: &EmailAddress,
) -> Result<Option<Found>, Internal> {
    let row = sqlx::query!(
        r#"SELECT role::text AS "role!", expires_at <= now() AS "expired!"
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
            role: Role::from_name(&row.role)?,
            expired: row.expired,
        })
    })
    .transpose()
}

/// Deletes the invitation `id` addressed to `email`, which locks it until the unit of work ends,
/// and returns whether there was one (DATA-20).
pub(super) async fn delete(
    uow: &mut UnitOfWork,
    id: uuid::Uuid,
    email: &EmailAddress,
) -> Result<bool, Internal> {
    let deleted = sqlx::query!(
        "DELETE FROM invitations WHERE id = $1 AND email = $2",
        id,
        email.as_str(),
    )
    .execute(uow.executor())
    .await?;
    Ok(deleted.rows_affected() > 0)
}

/// A page of the invitations of `workspace`, one whose only member is the user, which came from
/// their own memberships, as their export holds them, the latest first, after `after`, an
/// invitation's creation time and ID (ACCOUNTS-024, DATA-16).
pub(super) async fn exported(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    after: Option<(time::OffsetDateTime, uuid::Uuid)>,
) -> Result<Vec<ExportedInvitation>, Internal> {
    let rows = sqlx::query!(
        r#"SELECT id, email, role::text AS "role!", expires_at, created_at, updated_at
        FROM invitations
        WHERE workspace_id = $1 AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4"#,
        workspace.uuid(),
        after.map(|after| after.0),
        after.map(|after| after.1),
        PAGE,
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(ExportedInvitation {
                id: row.id,
                workspace_id: workspace,
                email: EmailAddress::from_stored(row.email),
                role: Role::from_name(&row.role)?,
                expires_at: Timestamp::from_database(row.expires_at),
                created_at: Timestamp::from_database(row.created_at),
                updated_at: Timestamp::from_database(row.updated_at),
            })
        })
        .collect()
}
