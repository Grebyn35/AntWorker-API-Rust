//! The personal data contract of `users` and of the user's exports (D31), which a deletion runs
//! last: it enqueues the email that tells the account's address it is deleted, locks the user's
//! row `FOR UPDATE`, then deletes it waiting for no lock, which the foreign keys carry to every row
//! that still names them, their exports' among them (the accounts spec's "Deleting an account").
//! An export holds the user's record (the accounts spec's "What an export holds").

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    mail::Mail,
    personal_data::{Column, ErasureError, ExportArchive, PersonalData},
};

use super::{
    emails,
    repo::{self, NO_WAIT},
};

/// The contract, with what sends the deletion's email.
pub(crate) struct Contract {
    mail: Mail,
}

impl Contract {
    /// The contract, sending its email through `mail`.
    pub(crate) fn new(mail: Mail) -> Self {
        Self { mail }
    }
}

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[
            Column::new("users", "email"),
            Column::new("users", "name"),
            Column::new("exports", "user_id"),
            Column::new("export_chunks", "bytes"),
        ];
        COLUMNS
    }

    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut ExportArchive,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(async move {
            let profile = repo::exported(uow, export.user()).await?;
            export.whole(uow, "accounts/profile.json", &[profile]).await
        })
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
            repo::lock(uow, user).await?;
            uow.wait_for_locks_at_most(NO_WAIT).await?;
            repo::delete(uow, user).await?;
            Ok(uow.wait_for_locks_as_before().await?)
        })
    }
}
