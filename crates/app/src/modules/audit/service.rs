//! Recording entries inside the unit of work of the change they record (D1, D6), and the
//! `SecurityEventSink` that records refused requests (AUDIT-004, the audit spec's "Recording a
//! refusal").

use platform::{
    authn::{AuditScope, BoxFuture},
    authz::{Refusal, SecurityEventSink},
    db::{Database, UnitOfWork},
    errors::Internal,
    telemetry,
};

use super::{audit, error::AuditError, model::Entry, repo};

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

/// Records each refusal in a unit of work of its own, since the refused operation wrote nothing.
pub(crate) struct Sink {
    database: Database,
}

impl Sink {
    /// The sink, which records in `database`.
    pub(crate) fn new(database: Database) -> Self {
        Self { database }
    }

    async fn write(&self, refusal: &Refusal) -> Result<(), Internal> {
        let mut uow = self.database.begin(refusal.client()).await?;
        let mut entry = Entry::new(
            audit::ACCESS_DENIED,
            audit::WORKSPACE,
            refusal.workspace_id().uuid(),
        );
        entry.details = serde_json::json!({ "permission": refusal.permission() });
        record(&mut uow, refusal.audit_scope(), entry)
            .await
            .map_err(|error| match error {
                AuditError::Internal(error) => error,
            })?;
        uow.commit().await
    }
}

impl SecurityEventSink for Sink {
    fn refused<'a>(&'a self, refusal: &'a Refusal) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(self.write(refusal))
    }
}
