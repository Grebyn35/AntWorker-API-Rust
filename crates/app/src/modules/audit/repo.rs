//! The SQL of `audit_entries` (ARCH-10).

use platform::{
    authn::{AuditScope, Log},
    db::UnitOfWork,
    errors::Internal,
};

use super::model::Entry;

/// Inserts `entry` in the log `scope` names, with the client of the unit of work and the trace
/// ID `trace_id` (AUDIT-001, AUDIT-003).
pub(super) async fn insert(
    uow: &mut UnitOfWork,
    scope: AuditScope,
    entry: &Entry,
    trace_id: &str,
) -> Result<(), Internal> {
    let (log, user, workspace) = match scope.log() {
        Log::User(user) => ("user", Some(user.uuid()), None),
        Log::Workspace(workspace) => ("workspace", None, Some(workspace.uuid())),
    };
    let client_ip = uow.client().map(|client| client.ip);
    let user_agent = uow.client().and_then(|client| client.user_agent.clone());
    sqlx::query!(
        "INSERT INTO audit_entries (
            scope, user_id, workspace_id, actor_user_id, target_user_id, action, resource_type,
            resource_id, details, client_ip, user_agent, trace_id
        ) VALUES ($1::text::audit_scope, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
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
    .await?;
    Ok(())
}
