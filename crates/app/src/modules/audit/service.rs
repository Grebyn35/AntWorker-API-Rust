//! Recording entries inside the unit of work of the change they record (D1, D6), the
//! `SecurityEventSink` that records refused requests (AUDIT-004, the audit spec's "Recording a
//! refusal"), and reading the logs (AUDIT-007, AUDIT-008).

use platform::{
    authn::{AuditScope, BoxFuture, Log, Principal},
    authz::{Authorized, Refusal, SecurityEventSink},
    db::{Database, UnitOfWork},
    errors::Internal,
    lists::{List, Page, key},
    telemetry,
    types::Client,
};

use serde::Serialize;

// part: superadmins
use super::model::Access;
// end part: superadmins
use super::{
    audit::{self, Denial},
    error::AuditError,
    model::{Entry, EntryFilter, LoggedEntry},
    repo::{self, Inserted},
};
use crate::modules::access::{self, AuditRead, AuditReadClients};

/// Records `entry` in the log that `scope` names, in `uow`, so it commits or rolls back with the
/// change, which fails with it, as an internal error (AUDIT-001). It refuses, as an internal
/// error, an entry unless its action declares both that kind of log and the entry's kind of
/// resource (AUDIT-013). The actor comes from the scope, the client from the unit of work, and
/// the trace ID from the current span (AUDIT-002, AUDIT-003).
pub(crate) async fn record<D: Serialize>(
    uow: &mut UnitOfWork,
    scope: AuditScope,
    entry: Entry<D>,
) -> Result<(), Internal> {
    let admitted = entry.admitted(scope)?;
    match repo::insert(uow, admitted, &telemetry::trace_id()).await? {
        Inserted::Recorded => Ok(()),
        Inserted::LogGone(refused) => Err(refused),
    }
}

/// A page of the log of the caller's workspace that `filter` keeps, the latest first: the entries
/// whose permission the caller's role holds, or that declare none, each with its client if the
/// role holds `audit.read_clients`, as `access` decides from the role the proof carries
/// (AUDIT-007, AUDIT-009, AUDIT-010, SEC-08).
pub(crate) async fn workspace_entries(
    uow: &mut UnitOfWork,
    authorized: &Authorized<AuditRead>,
    filter: &EntryFilter,
    page: &Page,
) -> Result<List<LoggedEntry>, AuditError> {
    let permissions = access::permissions(authorized)?;
    let clients = access::holds::<AuditReadClients, _>(authorized)?;
    let rows = repo::reading::workspace(
        uow,
        authorized.workspace_id(),
        &permissions,
        clients,
        filter,
        page,
    )
    .await?;
    Ok(page.list(rows, |row| key(row.created_at, row.id)))
}

/// A page of the entries that the caller reads as their own, which `filter` keeps, the latest
/// first: those of their own log, and those of a workspace's log whose target is them, each with
/// its client when it was theirs (AUDIT-008, AUDIT-009, AUDIT-010).
pub(crate) async fn own_entries(
    database: &Database,
    client: &Client,
    principal: &Principal,
    filter: &EntryFilter,
    page: &Page,
) -> Result<List<LoggedEntry>, AuditError> {
    let mut uow = database.begin(client, Some(principal)).await?;
    let rows = repo::reading::own(&mut uow, principal.user_id(), filter, page).await?;
    Ok(page.list(rows, |row| key(row.created_at, row.id)))
}

/// Records each refusal in a unit of work of its own, since the refused operation wrote nothing,
/// and in a second where the first finds the workspace's log gone (AUDIT-004).
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
        let entry = Entry::detailed(
            audit::ACCESS_DENIED,
            audit::WORKSPACE,
            refusal.workspace_id().uuid(),
            Denial {
                permission: Some(refusal.permission()),
                // part: superadmins
                access: None,
                // end part: superadmins
            },
        );
        let admitted = entry.admitted(scope)?;
        let mut uow = self.database.begin(refusal.client(), None).await?;
        match repo::insert(&mut uow, admitted, &telemetry::trace_id()).await? {
            Inserted::Recorded => {
                uow.commit().await?;
                Ok(true)
            }
            Inserted::LogGone(_) => Ok(false),
        }
    }
}

impl SecurityEventSink for Sink {
    fn refused<'a>(&'a self, refusal: &'a Refusal) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(self.write(refusal))
    }

    // part: superadmins
    /// Records the refusal in the caller's own log, with the deployment as its resource. A
    /// caller whose account went while the refused request ran has no log left to hold it, as
    /// a non-member's refusal has none (SUPERADMINS-004, AUDIT-004).
    fn refused_superadmin<'a>(
        &'a self,
        scope: AuditScope,
        client: &'a Client,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(async move {
            let entry = Entry {
                action: audit::ACCESS_DENIED,
                resource_type: audit::DEPLOYMENT,
                resource_id: None,
                target: None,
                details: Denial {
                    permission: None,
                    access: Some(Access::Superadmin),
                },
            };
            let admitted = entry.admitted(scope)?;
            let mut uow = self.database.begin(client, None).await?;
            match repo::insert(&mut uow, admitted, &telemetry::trace_id()).await? {
                Inserted::Recorded => uow.commit().await,
                Inserted::LogGone(_) => Ok(()),
            }
        })
    }
    // end part: superadmins
}
