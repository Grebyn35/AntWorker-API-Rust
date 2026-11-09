//! The SQL of `audit_entries` and `audit_entry_clients` (ARCH-10).

use platform::{
    authn::{AuditScope, Log},
    db::UnitOfWork,
    errors::Internal,
    personal_data::PAGE,
    types::{Timestamp, UserId, WorkspaceId},
};

use super::model::{Entry, ExportedEntry};

/// What inserting an entry came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Inserted {
    /// The entry is in its log.
    Recorded,
    /// Its log is a user's whose deletion committed first, so the log's foreign key refused the
    /// entry, and the unit of work can't go on.
    LogGone,
}

/// The foreign key of an entry's log of the user scope, which refuses it once the user is gone
/// (ERR-05).
const USER_LOG: &str = "audit_entries_user_id_fkey";

/// Whether `error` is [`USER_LOG`]'s refusal of an entry.
fn log_gone(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.constraint())
        .is_some_and(|constraint| constraint == USER_LOG)
}

/// Inserts `entry` in the log `scope` names, with the trace ID `trace_id`, and, in the same
/// statement, the client of the unit of work, if it has one, as the client of the entry's actor,
/// or, for an entry without one, of the user whose log holds it (AUDIT-001, AUDIT-003,
/// AUDIT-006).
pub(super) async fn insert(
    uow: &mut UnitOfWork,
    scope: AuditScope,
    entry: &Entry,
    trace_id: &str,
) -> Result<Inserted, Internal> {
    let (log, user, workspace) = match scope.log() {
        Log::User(user) => ("user", Some(user.uuid()), None),
        Log::Workspace(workspace) => ("workspace", None, Some(workspace.uuid())),
    };
    let client_ip = uow.client().map(|client| client.ip);
    let user_agent = uow.client().and_then(|client| client.user_agent.clone());
    let inserted = sqlx::query!(
        "WITH entry AS (
            INSERT INTO audit_entries (
                scope, user_id, workspace_id, actor_user_id, target_user_id, action, resource_type,
                resource_id, details, trace_id
            ) VALUES ($1::text::audit_scope, $2, $3, $4, $5, $6, $7, $8, $9, $12)
            RETURNING id, coalesce(actor_user_id, user_id) AS client_user_id
        )
        INSERT INTO audit_entry_clients (entry_id, user_id, client_ip, user_agent)
        SELECT id, client_user_id, $10, $11 FROM entry
        WHERE $10::inet IS NOT NULL OR $11::text IS NOT NULL",
        log,
        user,
        workspace,
        scope.actor().map(|actor| actor.uuid()),
        entry.target.map(|target| target.uuid()),
        entry.action.name(),
        entry.resource_type.name(),
        entry.resource_id,
        &entry.details,
        client_ip.map(ipnet::IpNet::from),
        user_agent,
        trace_id,
    )
    .execute(uow.executor())
    .await;
    match inserted {
        Ok(_) => Ok(Inserted::Recorded),
        Err(error) if log_gone(&error) => Ok(Inserted::LogGone),
        Err(error) => Err(error.into()),
    }
}

/// The columns of an entry that an export holds, with its client's when it was the user's.
struct Row {
    id: uuid::Uuid,
    workspace_id: Option<uuid::Uuid>,
    action: String,
    resource_type: String,
    resource_id: Option<uuid::Uuid>,
    actor_user_id: Option<uuid::Uuid>,
    target_user_id: Option<uuid::Uuid>,
    details: serde_json::Value,
    created_at: time::OffsetDateTime,
    client_ip: Option<ipnet::IpNet>,
    user_agent: Option<String>,
}

impl Row {
    fn exported(self) -> ExportedEntry {
        ExportedEntry {
            id: self.id,
            workspace_id: self.workspace_id.map(WorkspaceId::from_uuid),
            action: self.action,
            resource_type: self.resource_type,
            resource_id: self.resource_id,
            actor_user_id: self.actor_user_id.map(UserId::from_uuid),
            target_user_id: self.target_user_id.map(UserId::from_uuid),
            details: self.details,
            created_at: Timestamp::from_database(self.created_at),
            client_ip: self.client_ip.map(|ip| ip.addr().to_string()),
            user_agent: self.user_agent,
        }
    }
}

/// A page of the entries of `user`'s own log, the latest first, after `after`, an entry's time
/// and ID, each with its client when it was the user's, through the index on the log's user and
/// the entry's time (ACCOUNTS-024, AUDIT-006, DATA-16).
pub(super) async fn exported_own(
    uow: &mut UnitOfWork,
    user: UserId,
    after: Option<(time::OffsetDateTime, uuid::Uuid)>,
) -> Result<Vec<ExportedEntry>, Internal> {
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT entry.id, entry.workspace_id, entry.action, entry.resource_type,
            entry.resource_id, entry.actor_user_id, entry.target_user_id, entry.details,
            entry.created_at, client.client_ip AS "client_ip?", client.user_agent AS "user_agent?"
        FROM audit_entries AS entry
        LEFT JOIN audit_entry_clients AS client
            ON client.entry_id = entry.id AND client.user_id = $1
        WHERE entry.user_id = $1
            AND ($2::timestamptz IS NULL OR (entry.created_at, entry.id) < ($2, $3))
        ORDER BY entry.created_at DESC, entry.id DESC
        LIMIT $4"#,
        user.uuid(),
        after.map(|after| after.0),
        after.map(|after| after.1),
        PAGE,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows.into_iter().map(Row::exported).collect())
}

/// A page of the entries of the log of `workspace`, one whose only member is `user`, which came
/// from their own memberships, as `exported_own` gives them, through the index on the log's
/// workspace and the entry's time (ACCOUNTS-024, AUDIT-006, DATA-16).
pub(super) async fn exported_workspace(
    uow: &mut UnitOfWork,
    user: UserId,
    workspace: WorkspaceId,
    after: Option<(time::OffsetDateTime, uuid::Uuid)>,
) -> Result<Vec<ExportedEntry>, Internal> {
    let rows = sqlx::query_as!(
        Row,
        r#"SELECT entry.id, entry.workspace_id, entry.action, entry.resource_type,
            entry.resource_id, entry.actor_user_id, entry.target_user_id, entry.details,
            entry.created_at, client.client_ip AS "client_ip?", client.user_agent AS "user_agent?"
        FROM audit_entries AS entry
        LEFT JOIN audit_entry_clients AS client
            ON client.entry_id = entry.id AND client.user_id = $1
        WHERE entry.workspace_id = $2
            AND ($3::timestamptz IS NULL OR (entry.created_at, entry.id) < ($3, $4))
        ORDER BY entry.created_at DESC, entry.id DESC
        LIMIT $5"#,
        user.uuid(),
        workspace.uuid(),
        after.map(|after| after.0),
        after.map(|after| after.1),
        PAGE,
    )
    .fetch_all(uow.executor())
    .await?;
    Ok(rows.into_iter().map(Row::exported).collect())
}
