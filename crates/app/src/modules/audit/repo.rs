//! The SQL of `audit_entries` and `audit_entry_clients` (ARCH-10).

use platform::{
    authn::{AuditScope, Log},
    db::UnitOfWork,
    errors::Internal,
};

use super::model::Entry;

/// What inserting an entry came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Inserted {
    /// The entry is in its log.
    Recorded,
    /// Its log, its actor or its target is gone: a deletion of their account or workspace
    /// committed first, so its foreign key refused the entry, and the unit of work can't go on.
    Gone,
}

/// The foreign keys of an entry and of its client, which refuse it once what one of them names
/// is gone (ERR-05).
const NAMED: [&str; 5] = [
    "audit_entries_user_id_fkey",
    "audit_entries_workspace_id_fkey",
    "audit_entries_actor_user_id_fkey",
    "audit_entries_target_user_id_fkey",
    "audit_entry_clients_user_id_fkey",
];

/// Whether `error` is one of [`NAMED`]'s foreign keys refusing an entry.
fn names_one_gone(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|error| error.constraint())
        .is_some_and(|constraint| NAMED.contains(&constraint))
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
        Err(error) if names_one_gone(&error) => Ok(Inserted::Gone),
        Err(error) => Err(error.into()),
    }
}
