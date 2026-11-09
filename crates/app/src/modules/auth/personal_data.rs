//! The personal data contract of `identities`, `sessions` and `device_tokens` (D31): it deletes
//! the user's identities, sessions and device tokens before the deletion's last step locks the
//! user's row `FOR UPDATE`, since a sign-in locks an identity, and a refresh a session, before it
//! reads the user's row through a foreign key (the auth spec's "Deleting an account").

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
        const COLUMNS: &[Column] = &[
            Column::new("identities", "user_id"),
            Column::new("sessions", "user_id"),
            Column::new("device_tokens", "user_id"),
        ];
        COLUMNS
    }

    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async move { Ok(service::erase(uow, principal.user_id()).await?) })
    }
}
