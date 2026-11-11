//! What other modules ask of `accounts`, inside their unit of work, the caller's own record, and
//! renaming oneself.
// part: files
//! Setting and removing an avatar are in `avatar.rs`.
// end part: files

// part: files
pub(super) mod avatar;
// end part: files
pub(super) mod deletion;
pub(super) mod exports;

use platform::{
    authn::Principal,
    db::{Database, UnitOfWork},
    errors::RequestError,
    lists::Search,
    types::{Client, EmailAddress, UserId, Version},
};

use super::{
    audit,
    error::AccountsError,
    events as published,
    model::{Address, Current, DisplayName, PublicProfile, Rename},
    repo,
};
use crate::modules::{
    access,
    audit::{self as log, Entry},
};
// part: invitations
use super::model::Profile;
// end part: invitations

/// What creating a user came to (ACCOUNTS-004).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Creation {
    /// The user exists, with this ID.
    Created(UserId),
    /// Another user has the email, so nothing was written.
    EmailInUse,
}

/// What changing a user's email came to (AUTH-051).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EmailChange {
    /// The user's email is the new one, which they proved.
    Changed,
    /// Another user has the email: the unit of work is aborted, and rolls back as it is dropped.
    InUse,
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

/// `user_id`'s address, or `None` once the account is gone. Callers pass the ID of the caller,
/// or of the user a job runs for (DATA-16).
pub(crate) async fn address(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Option<Address>, AccountsError> {
    Ok(repo::address(uow, user_id).await?)
}

/// `user_id`'s address, as [`address`] gives it, with the user's row locked against every other
/// change until the unit of work ends, as a deletion of the account locks it first, so a check of
/// a code and a deletion take turns, and of two checks the second reads what the first left
/// (AUTH-031, DATA-20). Callers pass the caller's ID (DATA-16).
pub(crate) async fn address_for_update(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<Option<Address>, AccountsError> {
    Ok(repo::address_for_update(uow, user_id).await?)
}

/// Makes `email` the caller's address, which they proved with the code sent to it, so their
/// record's `email_verified` is true, and publishes that their record changed, as a rename does,
/// unless another user has the email (AUTH-051, ACCOUNTS-004). The caller holds the user's row
/// through [`address_for_update`]; the update of `email`, which has a unique index, holds it
/// `FOR UPDATE`, so the workspaces it publishes in are all of the user's until it commits
/// (DATA-20).
pub(crate) async fn change_email(
    uow: &mut UnitOfWork,
    principal: &Principal,
    email: &EmailAddress,
) -> Result<EmailChange, AccountsError> {
    if !repo::change_email(uow, principal.user_id(), email).await? {
        return Ok(EmailChange::InUse);
    }
    publish_updated(uow, principal).await?;
    Ok(EmailChange::Changed)
}

/// Records that `user_id`'s account proved its email address (AUTH-031): its record's
/// `email_verified` becomes true (ACCOUNTS-001). Callers pass the caller's ID (DATA-16).
pub(crate) async fn verify_email(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<(), AccountsError> {
    Ok(repo::verify_email(uow, user_id).await?)
}

/// The caller's own record, with its version, in a unit of work of its own (ACCOUNTS-001).
pub(crate) async fn me(
    database: &Database,
    client: &Client,
    principal: &Principal,
) -> Result<Current, AccountsError> {
    let mut uow = database.begin(client).await?;
    current(&mut uow, principal).await
}

// part: invitations
/// The caller's own record (ACCOUNTS-001).
pub(crate) async fn profile(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<Profile, AccountsError> {
    Ok(current(uow, principal).await?.profile)
}
// end part: invitations

/// The caller's own record, with its version, as ACCOUNTS-001 gives it, or 401 once a deletion
/// of their account has taken it, and their session with it (ACCOUNTS-019).
pub(super) async fn current(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<Current, AccountsError> {
    let current = repo::profile(uow, principal.user_id())
        .await?
        .ok_or(RequestError::Unauthenticated)?;
    // part: files
    let current = avatar::with_link(uow, current).await?;
    // end part: files
    Ok(current)
}

/// Renames the caller, in a unit of work of `client`'s request, when the record's version is one
/// of `versions`, or whatever it is when there are none, and answers the record as it then is:
/// the update, its audit entry in the caller's own log and its events commit together. The unit
/// of work first locks the caller's row, so the workspaces it publishes in are all of theirs
/// until it commits (ACCOUNTS-007, PLATFORM-071, DATA-20).
pub(crate) async fn rename(
    database: &Database,
    client: &Client,
    principal: &Principal,
    rename: Rename,
    versions: Option<&[Version]>,
) -> Result<Current, AccountsError> {
    let mut uow = database.begin(client).await?;
    let user = principal.user_id();
    lock_caller(&mut uow, principal).await?;
    if !repo::rename(&mut uow, user, &rename.name, versions).await? {
        return Err(RequestError::PreconditionFailed.into());
    }
    let entry = Entry::new(audit::USER_RENAMED, audit::USER, user.uuid());
    log::record(&mut uow, principal.audit_scope(), entry).await?;
    publish_updated(&mut uow, principal).await?;
    let current = current(&mut uow, principal).await?;
    uow.commit().await?;
    Ok(current)
}

/// Locks the caller's row `FOR UPDATE`, before any other statement of a change of their record,
/// or refuses with 401 once a deletion of their account that the lock waited for has taken it,
/// and their session with it, as a request sent after the deletion is refused (ACCOUNTS-019).
pub(super) async fn lock_caller(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<(), AccountsError> {
    if repo::lock(uow, principal.user_id()).await? {
        Ok(())
    } else {
        Err(RequestError::Unauthenticated.into())
    }
}

/// Publishes that the caller's record changed: `user.updated` to their own streams, and
/// `member.updated` in each workspace they belong to, whose members read the member again
/// (ACCOUNTS-007, the accounts spec's "Events").
pub(super) async fn publish_updated(
    uow: &mut UnitOfWork,
    principal: &Principal,
) -> Result<(), AccountsError> {
    let user = principal.user_id();
    let payload = published::UserUpdated { user_id: user };
    uow.publish_to_user(&published::USER_UPDATED, user, &payload)
        .await?;
    for workspace_id in access::workspace_ids(uow, principal).await? {
        let payload = published::MemberUpdated {
            workspace_id,
            user_id: user,
        };
        uow.publish_to_workspace(&published::MEMBER_UPDATED, workspace_id, &payload)
            .await?;
    }
    Ok(())
}

/// The names and emails of exactly the users `ids` names, which came from a query filtered by
/// tenant, such as a workspace's member list (ACCOUNTS-005, DATA-16).
pub(crate) async fn profiles(
    uow: &mut UnitOfWork,
    ids: &[UserId],
) -> Result<Vec<PublicProfile>, AccountsError> {
    let profiles = repo::profiles(uow, ids).await?;
    // part: files
    let profiles = avatar::with_links(uow, profiles).await?;
    // end part: files
    Ok(profiles)
}

/// The IDs among `ids`, passed under the same terms as to [`profiles`], of the users whose name
/// or email contains `search` (ACCOUNTS-006).
pub(crate) async fn matching(
    uow: &mut UnitOfWork,
    ids: &[UserId],
    search: &Search,
) -> Result<Vec<UserId>, AccountsError> {
    Ok(repo::matching(uow, ids, search).await?)
}
