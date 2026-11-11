//! The personal data contract of `identities`, `sessions`, `device_tokens`, `verification_codes`,
//! `totp_factors`, `totp_setups`, `recovery_codes` and `sign_in_challenges` (D31): it exports
//! them without their hashes and secrets (the accounts spec's "What an export holds"), and it
//! deletes the user's identities, sessions, device tokens, codes and second factors before
//! the deletion's last step locks the user's row `FOR UPDATE`, since a sign-in locks an identity,
//! and a refresh a session, before it reads the user's row through a foreign key, and a code's
//! job holds the code it issues until it commits (the auth spec's "Deleting an account").

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    personal_data::{Column, ErasureError, ExportArchive, PersonalData, next_after},
};

use super::{repo::exported, service};

/// The contract.
pub(crate) struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[
            Column::new("identities", "user_id"),
            Column::new("sessions", "user_id"),
            Column::new("sessions", "device_name"),
            Column::new("device_tokens", "user_id"),
            Column::new("verification_codes", "user_id"),
            Column::new("totp_factors", "user_id"),
            Column::new("totp_setups", "user_id"),
            Column::new("recovery_codes", "user_id"),
            Column::new("sign_in_challenges", "user_id"),
            Column::new("sign_in_challenges", "device_name"),
        ];
        COLUMNS
    }

    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut ExportArchive,
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
            export.finish(uow, document).await?;
            let codes = exported::verification_codes(uow, user).await?;
            export
                .whole(uow, "auth/verification_codes.json", &codes)
                .await?;
            let (apps, setups) = exported::apps(uow, user).await?;
            export.whole(uow, "auth/totp_factors.json", &apps).await?;
            export.whole(uow, "auth/totp_setups.json", &setups).await?;
            let recovery_codes = exported::recovery_codes(uow, user).await?;
            export
                .whole(uow, "auth/recovery_codes.json", &recovery_codes)
                .await?;
            let challenges = exported::challenges(uow, user).await?;
            export
                .whole(uow, "auth/sign_in_challenges.json", &challenges)
                .await
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
