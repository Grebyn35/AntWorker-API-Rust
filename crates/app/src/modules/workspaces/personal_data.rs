//! The personal data contract of `workspaces` (D31): it exports the workspaces whose only member
//! is the user (the accounts spec's "What an export holds"), and, once `access::erasable` has
//! locked the user's memberships, and found that no workspace they own has another member, it has
//! `access` end each membership, with its signal, then deletes each workspace they own, with
//! everything in it, and in each other workspace records and publishes that they left
//! (ACCOUNTS-013, ACCOUNTS-014, the workspaces spec's "Erasure").

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    jobs,
    personal_data::{Column, ErasureError, Export, PersonalData, pages},
    types::WorkspaceId,
};

use super::{audit, events as published, repo};
use crate::modules::{
    access::{self, AccessError, Role},
    audit::{self as log, AuditError, Entry},
};

/// The contract.
pub(crate) struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[Column::new("workspaces", "name")];
        COLUMNS
    }

    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut Export,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(async move {
            let sole = access::sole_workspaces(uow, export.user())
                .await
                .map_err(internal)?;
            let mut document = export.document("workspaces/workspaces.json");
            for ids in pages(&sole) {
                let page = repo::exported(uow, ids).await?;
                export.write(uow, &mut document, &page).await?;
            }
            export.finish(uow, document).await
        })
    }

    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async move {
            let held = access::held(uow, principal).await.map_err(internal)?;
            for (workspace, role) in held {
                access::leave(uow, principal, workspace)
                    .await
                    .map_err(internal)?;
                if role == Role::Owner {
                    // Its only member was the user.
                    repo::delete(uow, workspace).await?;
                    jobs::forget_workspace(uow, workspace).await?;
                } else {
                    left(uow, principal, workspace).await?;
                }
            }
            Ok(())
        })
    }
}

/// Records and publishes that the user left `workspace` (ACCOUNTS-014).
async fn left(
    uow: &mut UnitOfWork,
    principal: &Principal,
    workspace: WorkspaceId,
) -> Result<(), Internal> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the deletion of an account records its user leaving each workspace they shared (the workspaces spec)"
    )]
    let scope = principal.leaver_scope(workspace);
    let mut entry = Entry::new(audit::MEMBER_LEFT, audit::WORKSPACE, workspace.uuid());
    entry.target = Some(principal.user_id());
    log::record(uow, scope, entry)
        .await
        .map_err(|error| match error {
            AuditError::Internal(error) => error,
        })?;
    let payload = published::MemberLeft {
        workspace_id: workspace,
        user_id: principal.user_id(),
    };
    uow.publish_to_workspace(&published::MEMBER_LEFT, workspace, &payload)
        .await
}

fn internal(error: AccessError) -> Internal {
    match error {
        AccessError::Internal(error) => error,
    }
}
