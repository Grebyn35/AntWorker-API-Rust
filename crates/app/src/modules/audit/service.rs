//! Recording entries inside the unit of work of the change they record (D1, D6).

use platform::{authn::AuditScope, db::UnitOfWork, telemetry};

use super::{error::AuditError, model::Entry, repo};

/// Records `entry` in the log that `scope` names, in `uow`, so it commits or rolls back with the
/// change (AUDIT-001). The actor comes from the scope, the client from the unit of work, and the
/// trace ID from the current span (AUDIT-002, AUDIT-003).
pub(crate) async fn record(
    uow: &mut UnitOfWork,
    scope: AuditScope,
    entry: Entry,
) -> Result<(), AuditError> {
    Ok(repo::insert(uow, scope, &entry, &telemetry::trace_id()).await?)
}
