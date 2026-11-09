//! The personal data contract of `workspace_memberships` (D31): it deletes every membership of
//! the user that remains once the contract of `workspaces` has deleted the workspaces whose only
//! member they were, each with its access-change signal (the access spec's "Erasure").

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    personal_data::{Column, ErasureError, PersonalData},
};

use super::service;

/// The contract.
pub(crate) struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[Column::new("workspace_memberships", "user_id")];
        COLUMNS
    }

    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async move { Ok(service::leave_all(uow, principal.user_id()).await?) })
    }
}
