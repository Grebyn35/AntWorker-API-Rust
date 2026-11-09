//! The personal data contract of `audit_entries` and `audit_entry_clients` (D31), whose erasure
//! the foreign keys carry out as the user's row goes: they delete the user's own log, set to null
//! the actor and the target of each other entry that was them, and delete the client of each
//! entry they acted in, so `app` needs no `UPDATE` or `DELETE` on either table (AUDIT-005,
//! AUDIT-006).

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
            Column::new("audit_entries", "user_id"),
            Column::new("audit_entries", "actor_user_id"),
            Column::new("audit_entries", "target_user_id"),
            Column::new("audit_entry_clients", "user_id"),
            Column::new("audit_entry_clients", "client_ip"),
            Column::new("audit_entry_clients", "user_agent"),
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
