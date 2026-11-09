//! The personal data contract of `invitations` (D31). It exports the invitations of each
//! workspace whose only member is the user (the accounts spec's "What an export holds"). Its
//! erasure changes nothing: an invitation that a deleted user sent is the workspace's, and stays,
//! while its foreign key sets its `inviter_id` to null as the user's row goes, and an invitation
//! addressed to their email is the workspace's too (ACCOUNTS-018).

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    personal_data::{Column, ErasureError, Export, PersonalData, next_after},
};

use super::repo;
use crate::modules::access::{self, AccessError};

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

    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut Export,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(async move {
            let sole = access::sole_workspaces(uow, export.user())
                .await
                .map_err(|error| match error {
                    AccessError::Internal(error) => error,
                })?;
            let mut document = export.document("invitations/invitations.json");
            for workspace in sole {
                let mut after = None;
                loop {
                    let page = repo::exported(uow, workspace, after).await?;
                    export.write(uow, &mut document, &page).await?;
                    match next_after(&page) {
                        Some(last) => after = Some((last.created_at.at(), last.id)),
                        None => break,
                    }
                }
            }
            export.finish(uow, document).await
        })
    }

    fn erase<'a>(
        &'a self,
        _uow: &'a mut UnitOfWork,
        _principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async { Ok(()) })
    }
}
