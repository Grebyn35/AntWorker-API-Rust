//! The personal data contract of `workspace_memberships` (D31): it exports the user's memberships
//! (the accounts spec's "What an export holds"), and deletes every membership of the user that
//! remains once the contract of `workspaces` has deleted the workspaces whose only member they
//! were, each with its access-change signal (the access spec's "Erasure").

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    personal_data::{Column, ErasureError, Export, PersonalData, next_after},
};

use super::{repo, service};

/// The contract.
pub(crate) struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[Column::new("workspace_memberships", "user_id")];
        COLUMNS
    }

    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut Export,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(async move {
            let mut document = export.document("access/memberships.json");
            let mut after = None;
            loop {
                let page = repo::exported(uow, export.user(), after).await?;
                export.write(uow, &mut document, &page).await?;
                match next_after(&page) {
                    Some(last) => after = Some((last.created_at.at(), last.id)),
                    None => break,
                }
            }
            export.finish(uow, document).await
        })
    }

    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async move { Ok(service::leave_all(uow, principal.user_id()).await?) })
    }
}
