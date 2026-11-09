//! Deleting an account across every module, through their personal data contracts (D16, D31,
//! the accounts spec's "Deleting an account").

use std::sync::Arc;

use platform::{
    authn::Principal,
    db::UnitOfWork,
    mail::Mail,
    personal_data::{Column, ErasureError, PersonalData},
};

use crate::modules::{
    access,
    accounts::{
        error::AccountsError,
        personal_data::{COLUMNS, Contract},
    },
};

/// What deleting an account came to, which `auth` answers with codes of its own (AUTH-023).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The account is deleted, once the unit of work commits.
    Deleted,
    /// The user owns a workspace that has another member, so nothing was erased (ACCOUNTS-012).
    OwnsSharedWorkspace,
    /// A request of the user's changed their account while the deletion ran, so the unit of
    /// work rolls back (ACCOUNTS-016).
    AccountChanged,
}

/// Every module's personal data contract, in the order a deletion runs them, the contract of
/// `accounts` last.
pub(crate) struct Deletion {
    contracts: Vec<Arc<dyn PersonalData>>,
}

impl Deletion {
    /// The deletion that runs `contracts`, the other modules', in their order, then the contract
    /// of `accounts`, which sends its email through `mail`.
    pub(crate) fn new(mut contracts: Vec<Arc<dyn PersonalData>>, mail: Mail) -> Self {
        contracts.push(Arc::new(Contract::new(mail)));
        Self { contracts }
    }

    /// Deletes the caller's account in `uow`, the unit of work of `auth` that confirmed it
    /// (AUTH-023): `access` locks their memberships first and refuses while a workspace they own
    /// has another member, then each contract erases its part (ACCOUNTS-012 to ACCOUNTS-016).
    pub(crate) async fn delete(
        &self,
        uow: &mut UnitOfWork,
        principal: &Principal,
    ) -> Result<Outcome, AccountsError> {
        if !access::erasable(uow, principal).await? {
            return Ok(Outcome::OwnsSharedWorkspace);
        }
        for contract in &self.contracts {
            match contract.erase(uow, principal).await {
                Ok(()) => {}
                Err(ErasureError::Interrupted) => return Ok(Outcome::AccountChanged),
                Err(ErasureError::Internal(error)) => return Err(error.into()),
            }
        }
        Ok(Outcome::Deleted)
    }
}

/// Every column of the contracts in `contracts`, and of the contract of `accounts`, which
/// [`Deletion::new`] adds to them (DATA-23).
pub(crate) fn columns(contracts: &[Arc<dyn PersonalData>]) -> Vec<Column> {
    let mut columns: Vec<Column> = contracts
        .iter()
        .flat_map(|contract| contract.columns().iter().copied())
        .collect();
    columns.extend_from_slice(COLUMNS);
    columns
}
