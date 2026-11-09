//! Deleting an account across every module, through their personal data contracts (D16, D31,
//! the accounts spec's "Deleting an account").

use std::sync::Arc;

use platform::{
    authn::Principal,
    db::UnitOfWork,
    personal_data::{ErasureError, PersonalData},
};

use crate::modules::{
    access,
    accounts::{error::AccountsError, repo},
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
    /// The deletion that runs `contracts`, in their order, which the composition root registers,
    /// the contract of `accounts` last.
    pub(crate) fn new(contracts: Vec<Arc<dyn PersonalData>>) -> Self {
        Self { contracts }
    }

    /// Deletes the caller's account in `uow`, the unit of work of `auth` that confirmed it
    /// (AUTH-023): it first holds the user's row against every other change of it, `FOR NO KEY
    /// UPDATE`, then `access` locks their memberships and refuses while a workspace they own has
    /// another member, then each contract erases its part (ACCOUNTS-012 to ACCOUNTS-016, the
    /// accounts spec's "Deleting an account").
    pub(crate) async fn delete(
        &self,
        uow: &mut UnitOfWork,
        principal: &Principal,
    ) -> Result<Outcome, AccountsError> {
        repo::hold(uow, principal.user_id()).await?;
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
