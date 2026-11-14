//! The personal data contract of `workspace_files`, `user_files`, `avatars`, `workspace_pictures`
//! and `exported_files` (D31): it exports the user's own files, their avatar, and the files and
//! the picture of each workspace whose only member is them, holding their bytes for the export
//! (FILES-026), and it deletes
//! the user's own files and their avatar before the deletion's last step locks the user's row
//! `FOR UPDATE`, since deleting a file locks its row before the user's (the accounts spec's
//! "Deleting an account"); their bytes go as every unreferenced object's do (FILES-011). A file
//! they uploaded to a workspace that stays is the workspace's, and its foreign key sets its
//! `uploader_id` to null as the user's row goes (ACCOUNTS-017).

use platform::{
    authn::{BoxFuture, Principal},
    db::UnitOfWork,
    errors::Internal,
    personal_data::{Column, ErasureError, ExportArchive, PersonalData},
};

use super::{repo, service::exported};

/// The contract.
pub(crate) struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[
            Column::new("workspace_files", "name"),
            Column::new("workspace_files", "uploader_id"),
            Column::new("user_files", "user_id"),
            Column::new("user_files", "name"),
            Column::new("avatars", "user_id"),
            Column::new("exported_files", "user_id"),
            Column::new("exported_files", "path"),
        ];
        COLUMNS
    }

    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut ExportArchive,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(exported::export(uow, export))
    }

    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async move {
            let user = principal.user_id();
            repo::user::delete_all(uow, user).await?;
            repo::avatar::delete(uow, user).await?;
            Ok(())
        })
    }
}
