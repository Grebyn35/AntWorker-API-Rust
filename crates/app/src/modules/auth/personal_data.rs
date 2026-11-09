//! The personal data contract of `identities`, `sessions` and `device_tokens` (D31): it exports
//! them without their hashes (the accounts spec's "What an export holds"), and it deletes
//! the user's identities, sessions and device tokens before the deletion's last step locks the
//! user's row `FOR UPDATE`, since a sign-in locks an identity, and a refresh a session, before it
//! reads the user's row through a foreign key (the auth spec's "Deleting an account").

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    personal_data::{Column, ErasureError, Export, PersonalData, next_after},
};

use super::{repo::exported, service};

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

    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut Export,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(async move {
            let user = export.user();
            let identities = exported::identities(uow, user).await?;
            export
                .whole(uow, "auth/identities.json", &identities)
                .await?;
            let mut document = export.document("auth/sessions.json");
            let mut after = None;
            loop {
                let page = exported::sessions(uow, user, after).await?;
                export.write(uow, &mut document, &page).await?;
                match next_after(&page) {
                    Some(last) => after = Some((last.created_at.at(), last.id)),
                    None => break,
                }
            }
            export.finish(uow, document).await?;
            let mut document = export.document("auth/device_tokens.json");
            let mut after = None;
            loop {
                let page = exported::device_tokens(uow, user, after).await?;
                export.write(uow, &mut document, &page).await?;
                match next_after(&page) {
                    Some(last) => after = Some((last.created_at.at(), last.id)),
                    None => break,
                }
            }
            export.finish(uow, document).await
        })
    }

    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async move { Ok(service::erase(uow, principal.user_id()).await?) })
    }
}
