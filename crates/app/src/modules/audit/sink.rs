//! The `SecurityEventSink` that records refused requests (AUDIT-004, the audit spec's
//! "Recording a refusal").

use platform::{
    authn::BoxFuture,
    authz::{Refusal, SecurityEventSink},
    db::Database,
    errors::Internal,
};

use super::{
    model::{Action, Entry, Resource},
    service,
};

// `audit` declares its own action and kind of resource here, beside the one place that records
// them, since a module named `audit` can't hold an `audit.rs` of its own (clippy's
// `module_inception`).
#[expect(
    clippy::disallowed_methods,
    reason = "audit declares its own audit action here (the audit spec)"
)]
/// Authorization refused a workspace request (AUDIT-004).
const ACCESS_DENIED: Action = Action::new("access.denied");
#[expect(
    clippy::disallowed_methods,
    reason = "audit declares its own kind of resource here (the audit spec)"
)]
/// A workspace.
const WORKSPACE: Resource = Resource::new("workspace");

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
        let mut entry = Entry::new(ACCESS_DENIED, WORKSPACE, refusal.workspace_id().uuid());
        entry.details = serde_json::json!({ "permission": refusal.permission() });
        service::record(&mut uow, refusal.audit_scope(), entry)
            .await
            .map_err(|error| match error {
                super::AuditError::Internal(error) => error,
            })?;
        uow.commit().await
    }
}

impl SecurityEventSink for Sink {
    fn refused<'a>(&'a self, refusal: &'a Refusal) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(self.write(refusal))
    }
}
