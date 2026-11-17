//! The personal data contract of `client_telemetry` (D31): it exports the user's consent, if they
//! ever chose (the accounts spec's "What an export holds"). Its erasure changes nothing: the
//! foreign key deletes the user's row as their account goes (CLIENT_TELEMETRY-003).

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    personal_data::{Column, ErasureError, ExportArchive, PersonalData},
};

use super::repo;

/// The contract.
pub(crate) struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[Column::new("telemetry_consents", "user_id")];
        COLUMNS
    }

    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut ExportArchive,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(async move {
            let row = repo::exported(uow, export.user()).await?;
            let rows: Vec<_> = row.into_iter().collect();
            export
                .whole(uow, "client_telemetry/telemetry_consents.json", &rows)
                .await
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
