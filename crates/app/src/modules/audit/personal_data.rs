//! The personal data contract of `audit_entries` and `audit_entry_clients` (D31). It exports the
//! user's own log, and the log of each workspace whose only member is the user, with the client of
//! each entry that was theirs (the accounts spec's "What an export holds"). The foreign keys carry
//! out its erasure as the user's row goes: they delete the user's own log, set to null the actor
//! and the target of each other entry that was them, and delete the client of each entry they
//! acted in, so `app` needs no `UPDATE` or `DELETE` on either table (AUDIT-005, AUDIT-006).

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    personal_data::{Column, ErasureError, Export, PersonalData, next_after},
    types::WorkspaceId,
};

use super::repo;
use crate::modules::access::{self, AccessError};

/// The contract.
pub(crate) struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[
            Column::new("audit_entries", "user_id"),
            Column::new("audit_entries", "actor_user_id"),
            Column::new("audit_entries", "target_user_id"),
            Column::new("audit_entry_clients", "user_id"),
            Column::new("audit_entry_clients", "client_ip"),
            Column::new("audit_entry_clients", "user_agent"),
        ];
        COLUMNS
    }

    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut Export,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(async move {
            let mut document = export.document("audit/log.json");
            log(uow, export, &mut document, None).await?;
            export.finish(uow, document).await?;
            let sole = access::sole_workspaces(uow, export.user())
                .await
                .map_err(|error| match error {
                    AccessError::Internal(error) => error,
                })?;
            let mut document = export.document("audit/workspace_logs.json");
            for workspace in sole {
                log(uow, export, &mut document, Some(workspace)).await?;
            }
            export.finish(uow, document).await
        })
    }

    fn erase<'a>(
        &'a self,
        _uow: &'a mut UnitOfWork,
        _principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Writes into `document` every entry of the user's own log, or of `workspace`'s, a page at a
/// time.
async fn log(
    uow: &mut UnitOfWork,
    export: &Export,
    document: &mut platform::personal_data::Document,
    workspace: Option<WorkspaceId>,
) -> Result<(), Internal> {
    let mut after = None;
    loop {
        let page = match workspace {
            None => repo::exported_own(uow, export.user(), after).await?,
            Some(workspace) => {
                repo::exported_workspace(uow, export.user(), workspace, after).await?
            }
        };
        export.write(uow, document, &page).await?;
        match next_after(&page) {
            Some(last) => after = Some((last.created_at.at(), last.id)),
            None => return Ok(()),
        }
    }
}
