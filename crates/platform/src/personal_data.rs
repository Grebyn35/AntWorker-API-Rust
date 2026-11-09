//! The personal data contract (D31): what each module that keeps personal data declares, and how
//! it erases a user's when their account is deleted, which the composition root registers in the
//! order a deletion runs them (the platform spec's "Personal data"). `platform` implements it for
//! its own tables, as [`Contract`].

use crate::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    jobs,
};

/// A column that references `users`, or that its migration comments `personal_data` (DATA-23).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Column {
    /// Its table.
    pub table: &'static str,
    /// Its name.
    pub name: &'static str,
}

impl Column {
    /// The column `name` of `table`.
    pub const fn new(table: &'static str, name: &'static str) -> Self {
        Self { table, name }
    }
}

/// Why an erasure stopped, which rolls its unit of work back.
#[derive(Debug, thiserror::Error)]
pub enum ErasureError {
    /// A lock that the erasure waited for longer than its unit of work allows, or a row that a
    /// request of the user's added while it ran, interrupted it, so the deletion is sent again
    /// (ACCOUNTS-016).
    #[error("the account changed while it was being deleted")]
    Interrupted,
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

/// A module's personal data contract (D31), in its `personal_data.rs`.
pub trait PersonalData: Send + Sync {
    /// Every column of the module's tables that references `users`, or that its migration
    /// comments `personal_data`.
    fn columns(&self) -> &'static [Column];

    /// Erases what the module keeps of the user that `principal` proves, in the unit of work of
    /// their account's deletion, as the module's spec says: it deletes rows, or leaves them to
    /// the foreign keys that delete them, or set to null what names the user, as the user's row
    /// goes.
    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>>;
}

/// The contract of `platform`'s own tables: the jobs enqueued for the user, whose payloads may
/// hold personal data, such as an email's address, which its erasure deletes, and their events
/// and idempotency records, which go with the user's row through their foreign keys (DATA-23).
pub struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[
            Column::new("events", "user_id"),
            Column::new("idempotency_records", "user_id"),
            Column::new("idempotency_records", "result"),
            Column::new("jobs", "payload"),
        ];
        COLUMNS
    }

    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async move { Ok(jobs::forget_user(uow, principal.user_id()).await?) })
    }
}
