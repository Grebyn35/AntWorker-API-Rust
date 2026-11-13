//! Recording entries inside the unit of work of the change they record (D1, D6), and the
//! `SecurityEventSink` that records refused requests (AUDIT-004, the audit spec's "Recording a
//! refusal").

use platform::{
    authn::{AuditScope, BoxFuture, Log},
    authz::{Refusal, SecurityEventSink},
    db::{Database, UnitOfWork},
    errors::Internal,
    telemetry,
};

use super::{
    audit,
    error::AuditError,
    model::Entry,
    repo::{self, Inserted},
};

/// Records `entry` in the log that `scope` names, in `uow`, so it commits or rolls back with the
/// change (AUDIT-001). The actor comes from the scope, the client from the unit of work, and the
/// trace ID from the current span (AUDIT-002, AUDIT-003).
pub(crate) async fn record(
    uow: &mut UnitOfWork,
    scope: AuditScope,
    entry: Entry,
) -> Result<(), AuditError> {
    match repo::insert(uow, scope, &entry, &telemetry::trace_id()).await? {
        Inserted::Recorded => Ok(()),
        Inserted::LogGone => Err(Internal::new("the audit entry's log is gone").into()),
    }
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
        let scope = refusal.audit_scope();
        if self.insert(refusal, scope).await? || matches!(scope.log(), Log::User(_)) {
            // Recorded, or the non-member's own log went with their account while the refused
            // request ran, so the refusal has nowhere to be recorded (AUDIT-004).
            return Ok(());
        }
        // The workspace's log went with the workspace while the refused request ran. The caller
        // is no member of it any more, so their own log holds the refusal, as a non-member's
        // does, unless their account went too (AUDIT-004).
        self.insert(refusal, refusal.own_scope()).await?;
        Ok(())
    }

    /// Records `refusal` in the log that `scope` names, in a unit of work of its own, and
    /// returns whether the log was there to hold it.
    async fn insert(&self, refusal: &Refusal, scope: AuditScope) -> Result<bool, Internal> {
        let mut uow = self.database.begin(refusal.client()).await?;
        let mut entry = Entry::new(
            audit::ACCESS_DENIED,
            audit::WORKSPACE,
            refusal.workspace_id().uuid(),
        );
        entry.details = serde_json::json!({ "permission": refusal.permission() });
        match repo::insert(&mut uow, scope, &entry, &telemetry::trace_id()).await? {
            Inserted::Recorded => {
                uow.commit().await?;
                Ok(true)
            }
            Inserted::LogGone => Ok(false),
        }
    }
}

impl SecurityEventSink for Sink {
    fn refused<'a>(&'a self, refusal: &'a Refusal) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(self.write(refusal))
    }
}
