//! The personal data contract of `invitations` (D31). Its erasure changes nothing: an invitation
//! that a deleted user sent is the workspace's, and stays, while its foreign key sets its
//! `inviter_id` to null as the user's row goes, and an invitation addressed to their email is the
//! workspace's too (ACCOUNTS-014).

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    personal_data::{Column, ErasureError, PersonalData},
};

/// The contract.
pub(crate) struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[
            Column::new("invitations", "email"),
            Column::new("invitations", "inviter_id"),
        ];
        COLUMNS
    }

    fn erase<'a>(
        &'a self,
        _uow: &'a mut UnitOfWork,
        _principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async { Ok(()) })
    }
}
