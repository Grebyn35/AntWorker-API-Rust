//! Who is a superadmin: the `SuperadminPolicy` that `platform::authz` asks on every route of a
//! superadmin's, the caller's own standing, and the operator's subcommand, which alone makes a
//! superadmin, which ends one, and which lists them (the superadmins spec).

use std::collections::HashMap;

use platform::{
    authn::{AuditScope, BoxFuture, Principal},
    authz::{Admission, SuperadminPolicy},
    db::{Database, UnitOfWork},
    errors::Internal,
    types::{Client, EmailAddress, UserId},
};

use super::{
    audit,
    error::SuperadminsError,
    model::{Change, Listed, Refusal, SuperadminStanding},
    repo,
};
use crate::modules::{
    accounts::{self, AccountsError},
    audit::{self as log, Entry},
    auth::{self, AuthError},
};

/// Decides by the caller's row, and by their account's authenticator app.
pub(crate) struct Policy;

impl SuperadminPolicy for Policy {
    fn admits<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        user: UserId,
    ) -> BoxFuture<'a, Result<Admission, Internal>> {
        Box::pin(async move {
            if !repo::is_one(uow, user).await? {
                return Ok(Admission::Refused);
            }
            // Every superadmin must have a second factor, which holds here, whatever became of
            // the account's app since the operator made it one (D63, SUPERADMINS-005).
            match auth::has_authenticator_app(uow, user).await {
                Ok(true) => Ok(Admission::Admitted),
                Ok(false) => Ok(Admission::WithoutSecondFactor),
                Err(AuthError::Internal(error)) => Err(error),
                Err(other) => Err(Internal::new(other)),
            }
        })
    }
}

/// Whether the caller is a superadmin, read by their own ID (SUPERADMINS-006).
pub(super) async fn standing(
    database: &Database,
    client: &Client,
    principal: &Principal,
) -> Result<SuperadminStanding, SuperadminsError> {
    let mut uow = database.begin(client, Some(principal)).await?;
    let superadmin = repo::is_one(&mut uow, principal.user_id()).await?;
    Ok(SuperadminStanding { superadmin })
}

/// Makes the account that holds `email` a superadmin, or ends its being one, as `change` says,
/// in one unit of work on `database`, which records it in the account's own log with no actor;
/// or reports why it changed nothing. With `email_on`, only an account that has proved the
/// address becomes one (SUPERADMINS-001 to SUPERADMINS-003).
pub(crate) async fn change(
    database: &Database,
    email_on: bool,
    change: Change,
    email: &EmailAddress,
) -> Result<Result<(), Refusal>, Internal> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the operator's subcommand acts for no request and no job (ARCH-14)"
    )]
    let mut uow = database.for_operator().await?;
    // The user's row first, as the user's own changes and a deletion of the account lock it
    // (DATA-20). An account that a deletion took, or that changed its address, while the lock
    // waited holds the address no more.
    let holder =
        accounts::holder_for_update(&mut uow, email)
            .await
            .map_err(|error| match error {
                AccountsError::Internal(error) => error,
                other => Internal::new(other),
            })?;
    let Some((user, address)) = holder else {
        return Ok(Err(Refusal::NoAccount));
    };
    let action = match change {
        Change::Add if email_on && !address.verified => {
            return Ok(Err(Refusal::AddressUnproved));
        }
        Change::Add if !repo::insert(&mut uow, user).await? => {
            return Ok(Err(Refusal::AlreadySuperadmin));
        }
        Change::Add => audit::SUPERADMIN_ADDED,
        Change::Remove if !repo::delete(&mut uow, user).await? => {
            return Ok(Err(Refusal::NotSuperadmin));
        }
        Change::Remove => audit::SUPERADMIN_REMOVED,
    };
    let entry = Entry::new(action, audit::USER, user.uuid());
    log::record(&mut uow, AuditScope::attempt(user), entry).await?;
    uow.commit().await?;
    Ok(Ok(()))
}

/// The deployment's superadmins, the one made first first, each with the address of their
/// account, for the subcommand's list, read in one unit of work on `database` that changes
/// nothing (SUPERADMINS-008).
pub(crate) async fn listed(database: &Database) -> Result<Vec<Listed>, Internal> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the operator's subcommand acts for no request and no job (ARCH-14)"
    )]
    let mut uow = database.for_operator().await?;
    let rows = repo::all(&mut uow).await?;
    let users: Vec<UserId> = rows.iter().map(|(user, _)| *user).collect();
    let mut emails: HashMap<UserId, EmailAddress> = accounts::profiles(&mut uow, &users)
        .await
        .map_err(|error| match error {
            AccountsError::Internal(error) => error,
            other => Internal::new(other),
        })?
        .into_iter()
        .map(|profile| (profile.id, profile.email))
        .collect();
    // A superadmin whose account a deletion took between the two reads is one no more.
    Ok(rows
        .into_iter()
        .filter_map(|(user, since)| {
            let email = emails.remove(&user)?;
            Some(Listed { user, email, since })
        })
        .collect())
}
