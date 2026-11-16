//! The reads of the logs' lists: a workspace's log, and a user's own (AUDIT-007, AUDIT-008).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    lists::Page,
    types::{Timestamp, UserId, WorkspaceId},
};

use crate::modules::audit::model::{EntryClient, EntryFilter, LoggedEntry};

/// The columns of an entry that a list shows, with its client's when the read joins it.
struct Row {
    id: uuid::Uuid,
    workspace_id: Option<uuid::Uuid>,
    action: String,
    resource_type: String,
    resource_id: Option<uuid::Uuid>,
    actor_user_id: Option<uuid::Uuid>,
    target_user_id: Option<uuid::Uuid>,
    details: serde_json::Value,
    trace_id: Option<String>,
    created_at: time::OffsetDateTime,
    client_ip: Option<ipnet::IpNet>,
    user_agent: Option<String>,
}

impl Row {
    fn logged(self) -> LoggedEntry {
        // Every client's row holds its IP address, so a joined one has it (the audit spec's
        // Owns).
        let client = self.client_ip.map(|ip| EntryClient {
            ip: ip.addr().to_string(),
            user_agent: self.user_agent,
        });
        LoggedEntry {
            id: self.id,
            workspace_id: self.workspace_id.map(WorkspaceId::from_uuid),
            action: self.action,
            resource_type: self.resource_type,
            resource_id: self.resource_id,
            actor_user_id: self.actor_user_id.map(UserId::from_uuid),
            target_user_id: self.target_user_id.map(UserId::from_uuid),
            details: self.details,
            trace_id: self.trace_id,
            created_at: Timestamp::from_database(self.created_at),
            client,
        }
    }
}

/// A filter's values, as a query binds them: `None` for a filter not given.
struct Bound {
    actions: Option<Vec<String>>,
    resource_types: Option<Vec<String>>,
    resource_ids: Option<Vec<uuid::Uuid>>,
    actors: Option<Vec<uuid::Uuid>>,
    targets: Option<Vec<uuid::Uuid>>,
    from: Option<time::OffsetDateTime>,
    to: Option<time::OffsetDateTime>,
}

impl Bound {
    fn of(filter: &EntryFilter) -> Self {
        let ids = |users: &[UserId]| users.iter().map(|user| user.uuid()).collect();
        Self {
            actions: filter.actions.as_ref().map(|actions| {
                actions
                    .iter()
                    .map(|action| action.as_str().to_owned())
                    .collect()
            }),
            resource_types: filter.resource_types.as_ref().map(|types| {
                types
                    .iter()
                    .map(|resource| resource.as_str().to_owned())
                    .collect()
            }),
            resource_ids: filter.resource_ids.clone(),
            actors: filter.actors.as_deref().map(ids),
            targets: filter.targets.as_deref().map(ids),
            from: filter.from.map(Timestamp::at),
            to: filter.to.map(Timestamp::at),
        }
    }
}

/// The page `page` of the entries of `workspace`'s log that `filter` keeps, the latest first:
/// those whose action declares no permission, and those whose permission is one of
/// `permissions`, each with its client when `clients` says so, through the index on the log's
/// workspace and the entry's time (AUDIT-007, AUDIT-009, AUDIT-010, DATA-16, DATA-17).
pub(in crate::modules::audit) async fn workspace(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    permissions: &[&str],
    clients: bool,
    filter: &EntryFilter,
    page: &Page,
) -> Result<Vec<LoggedEntry>, Internal> {
    let bound = Bound::of(filter);
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT entry.id, entry.workspace_id, entry.action, entry.resource_type,
            entry.resource_id, entry.actor_user_id, entry.target_user_id, entry.details,
            entry.trace_id, entry.created_at,
            client.client_ip AS "client_ip?", client.user_agent AS "user_agent?"
        FROM audit_entries AS entry
        LEFT JOIN audit_entry_clients AS client
            ON client.entry_id = entry.id AND $3::boolean
        WHERE entry.workspace_id = $1
            AND (entry.permission IS NULL OR entry.permission = ANY($2::text[]))
            AND ($4::text[] IS NULL OR entry.action = ANY($4))
            AND ($5::text[] IS NULL OR entry.resource_type = ANY($5))
            AND ($6::uuid[] IS NULL OR entry.resource_id = ANY($6))
            AND ($7::uuid[] IS NULL OR entry.actor_user_id = ANY($7))
            AND ($8::uuid[] IS NULL OR entry.target_user_id = ANY($8))
            AND ($9::timestamptz IS NULL OR entry.created_at >= $9)
            AND ($10::timestamptz IS NULL OR entry.created_at < $10)
            AND ($11::timestamptz IS NULL OR (entry.created_at, entry.id) < ($11, $12))
        ORDER BY entry.created_at DESC, entry.id DESC
        LIMIT $13"#,
        workspace.uuid(),
        permissions as &[&str],
        clients,
        bound.actions.as_deref(),
        bound.resource_types.as_deref(),
        bound.resource_ids.as_deref(),
        bound.actors.as_deref(),
        bound.targets.as_deref(),
        bound.from,
        bound.to,
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows.into_iter().map(Row::logged).collect())
}

/// The page `page` of the entries that `user` reads as their own, which `filter` keeps, the
/// latest first: those of their own log, through the index on the log's user and the entry's
/// time, and those of a workspace's log whose target is them, through the index on the target
/// and the entry's time, each read limited to the page and merged; each with its client when it
/// was theirs (AUDIT-008, AUDIT-009, AUDIT-010, DATA-16, DATA-17).
pub(in crate::modules::audit) async fn own(
    uow: &mut UnitOfWork,
    user: UserId,
    filter: &EntryFilter,
    page: &Page,
) -> Result<Vec<LoggedEntry>, Internal> {
    let bound = Bound::of(filter);
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT entry.id AS "id!", entry.workspace_id, entry.action AS "action!",
            entry.resource_type AS "resource_type!", entry.resource_id, entry.actor_user_id,
            entry.target_user_id, entry.details AS "details!", entry.trace_id,
            entry.created_at AS "created_at!",
            client.client_ip AS "client_ip?", client.user_agent AS "user_agent?"
        FROM (
            (SELECT id, workspace_id, action, resource_type, resource_id, actor_user_id,
                target_user_id, details, trace_id, created_at
            FROM audit_entries
            WHERE user_id = $1
                AND ($2::text[] IS NULL OR action = ANY($2))
                AND ($3::text[] IS NULL OR resource_type = ANY($3))
                AND ($4::uuid[] IS NULL OR resource_id = ANY($4))
                AND ($5::uuid[] IS NULL OR actor_user_id = ANY($5))
                AND ($6::uuid[] IS NULL OR target_user_id = ANY($6))
                AND ($7::timestamptz IS NULL OR created_at >= $7)
                AND ($8::timestamptz IS NULL OR created_at < $8)
                AND ($9::timestamptz IS NULL OR (created_at, id) < ($9, $10))
            ORDER BY created_at DESC, id DESC
            LIMIT $11)
            UNION ALL
            (SELECT id, workspace_id, action, resource_type, resource_id, actor_user_id,
                target_user_id, details, trace_id, created_at
            FROM audit_entries
            WHERE target_user_id = $1 AND scope = 'workspace'
                AND ($2::text[] IS NULL OR action = ANY($2))
                AND ($3::text[] IS NULL OR resource_type = ANY($3))
                AND ($4::uuid[] IS NULL OR resource_id = ANY($4))
                AND ($5::uuid[] IS NULL OR actor_user_id = ANY($5))
                AND ($6::uuid[] IS NULL OR target_user_id = ANY($6))
                AND ($7::timestamptz IS NULL OR created_at >= $7)
                AND ($8::timestamptz IS NULL OR created_at < $8)
                AND ($9::timestamptz IS NULL OR (created_at, id) < ($9, $10))
            ORDER BY created_at DESC, id DESC
            LIMIT $11)
        ) AS entry
        LEFT JOIN audit_entry_clients AS client
            ON client.entry_id = entry.id AND client.user_id = $1
        ORDER BY entry.created_at DESC, entry.id DESC
        LIMIT $11"#,
        user.uuid(),
        bound.actions.as_deref(),
        bound.resource_types.as_deref(),
        bound.resource_ids.as_deref(),
        bound.actors.as_deref(),
        bound.targets.as_deref(),
        bound.from,
        bound.to,
        page.after_at(),
        page.after_id(),
        page.fetch(),
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows.into_iter().map(Row::logged).collect())
}
