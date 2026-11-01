//! What other modules ask of `accounts`, inside their unit of work, and the caller's own record.

use platform::{
    authn::Principal,
    db::{Database, UnitOfWork},
    errors::Internal,
    types::{Client, EmailAddress, UserId},
};

use super::{
    error::AccountsError,
    model::{DisplayName, Profile},
    repo,
};

/// What creating a user came to (ACCOUNTS-004).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Creation {
    /// The user exists, with this ID.
    Created(UserId),
    /// Another user has the email, so nothing was written.
    EmailInUse,
}

/// Creates a user with `email` and `name`, unless another user has the email (ACCOUNTS-004).
pub(crate) async fn create(
    uow: &mut UnitOfWork,
    email: &EmailAddress,
    name: &DisplayName,
) -> Result<Creation, AccountsError> {
    Ok(match repo::insert(uow, email, name).await? {
        Some(id) => Creation::Created(id),
        None => Creation::EmailInUse,
    })
}

/// The ID of the user with `email`, if there is one.
pub(crate) async fn find_by_email(
    uow: &mut UnitOfWork,
    email: &EmailAddress,
) -> Result<Option<UserId>, AccountsError> {
    Ok(repo::find_by_email(uow, email).await?)
}

/// The caller's own record, in a unit of work of its own (ACCOUNTS-001).
pub(crate) async fn me(
    database: &Database,
    client: &Client,
    principal: &Principal,
) -> Result<Profile, AccountsError> {
    let mut uow = database.begin(client).await?;
    profile(&mut uow, principal).await
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
