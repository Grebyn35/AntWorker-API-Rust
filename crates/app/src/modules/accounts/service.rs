//! What other modules ask of `accounts`, inside their unit of work.

use platform::{
    authn::Principal,
    db::UnitOfWork,
    errors::Internal,
    types::{EmailAddress, UserId},
};

use super::{
    error::AccountsError,
    model::{DisplayName, Profile},
    repo,
};

/// Creates a user with `email` and `name`, or reports `accounts.email_in_use` (ACCOUNTS-004).
pub(crate) async fn create(
    uow: &mut UnitOfWork,
    email: &EmailAddress,
    name: &DisplayName,
) -> Result<UserId, AccountsError> {
    repo::insert(uow, email, name)
        .await?
        .ok_or(AccountsError::EmailInUse)
}

/// The ID of the user with `email`, if there is one.
pub(crate) async fn find_by_email(
    uow: &mut UnitOfWork,
    email: &EmailAddress,
) -> Result<Option<UserId>, Internal> {
    repo::find_by_email(uow, email).await
}

/// The caller's own record (ACCOUNTS-001).
pub(crate) async fn profile(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<Profile, AccountsError> {
    repo::profile(uow, principal.user_id())
        .await?
        .ok_or_else(|| Internal::new("the caller's user doesn't exist").into())
}
