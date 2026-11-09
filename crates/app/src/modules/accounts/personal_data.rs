//! The personal data contract of `users` (D31), which a deletion runs last: it enqueues the email
//! that tells the account's address it is deleted, then lets the unit of work wait at most 200
//! milliseconds for a lock, locks the user's row, and deletes it, which the foreign keys carry to
//! every row that still names them (the accounts spec's "Deleting an account").

use std::time::Duration;

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    mail::Mail,
    personal_data::{Column, ErasureError, PersonalData},
};

use super::{emails, repo};

/// The longest that the deletion's last step waits for a lock, well within Postgres's
/// `deadlock_timeout` of 1 second, so a deletion that requests of the user's own would make wait
/// on each other yields first (ACCOUNTS-016).
const LAST_STEP_WAITS_AT_MOST: Duration = Duration::from_millis(200);

/// The contract, with what sends the deletion's email.
pub(crate) struct Contract {
    mail: Mail,
}

impl Contract {
    /// The contract, sending its email through `mail`.
    pub(super) fn new(mail: Mail) -> Self {
        Self { mail }
    }
}

/// The columns of `users` that hold personal data, its rows being the users themselves.
pub(super) const COLUMNS: &[Column] =
    &[Column::new("users", "email"), Column::new("users", "name")];

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        COLUMNS
    }

    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async move {
            let user = principal.user_id();
            let current = repo::profile(uow, user)
                .await?
                .ok_or_else(|| Internal::new("the caller's user doesn't exist"))?;
            let email = emails::Deleted {
                to: current.profile.email,
            }
            .render()?;
            #[expect(
                clippy::disallowed_methods,
                reason = "the deletion's email runs for no one, once the user is erased (the accounts spec)"
            )]
            let scope = principal.erased_job_scope();
            self.mail.send(uow, scope, email).await?;
            uow.wait_for_locks_at_most(LAST_STEP_WAITS_AT_MOST).await?;
            repo::delete(uow, user).await?;
            Ok(uow.wait_for_locks_as_before().await?)
        })
    }
}
