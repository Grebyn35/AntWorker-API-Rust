//! The operator's removal of a user's authenticator app, for a user who lost it and their
//! recovery codes, which the binary's subcommand `remove-second-factor` runs (AUTH-072, D135).

use platform::{
    authn::AuditScope, db::Database, errors::Internal, jobs::JobScope, mail::Mail,
    types::EmailAddress,
};

use super::FactorChange;
use crate::modules::{
    accounts,
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        emails::FactorChangedEmail,
        error::AuthError,
        repo::{self, second_factors as factors},
        service::sessions::end_sessions,
    },
};

/// What the operator's removal of a second factor found (AUTH-072).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    /// The account's app is removed.
    Removed,
    /// No account holds the address.
    NoAccount,
    /// The account that holds the address has no app.
    NoApp,
}

/// Removes the authenticator app and the recovery codes of the account that holds `email`, ends
/// every session of its user, each with its access-change signal, and deletes their sign-in
/// challenges, in one unit of work on `database`, which records `second_factor.removed` in the
/// account's log, with no actor, and enqueues, through `mail`, the email that tells the address
/// (AUTH-072, AUTH-068). An address that no account holds, or an account without an app, changes
/// nothing. It locks the user's row, then the app, then the challenges and the sessions, the order
/// of every operation that ends sessions, so it takes turns with the user's own requests
/// (DATA-20).
pub(crate) async fn remove_second_factor(
    database: &Database,
    mail: &Mail,
    email: &EmailAddress,
) -> Result<Removal, Internal> {
    remove(database, mail, email)
        .await
        .map_err(|error| match error {
            AuthError::Internal(error) => error,
            other => Internal::new(other),
        })
}

/// [`remove_second_factor`], whose steps report `auth`'s errors, which are only ever internal
/// here.
async fn remove(
    database: &Database,
    mail: &Mail,
    email: &EmailAddress,
) -> Result<Removal, AuthError> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the operator's subcommand acts for no request and no job (ARCH-14)"
    )]
    let mut uow = database.for_operator().await?;
    let Some(user_id) = accounts::find_by_email(&mut uow, email).await? else {
        return Ok(Removal::NoAccount);
    };
    // The user's row first, as a deletion of the account and the user's own changes lock it.
    let Some(address) = accounts::address_for_update(&mut uow, user_id).await? else {
        return Ok(Removal::NoAccount);
    };
    let Some(id) = factors::delete_app(&mut uow, user_id).await? else {
        return Ok(Removal::NoApp);
    };
    factors::delete_recovery_codes(&mut uow, user_id).await?;
    factors::delete_challenges(&mut uow, user_id).await?;
    let scope = AuditScope::attempt(user_id);
    let ended = repo::delete_sessions(&mut uow, user_id).await?;
    end_sessions(&mut uow, scope, user_id, &ended).await?;
    audit_log::record(
        &mut uow,
        scope,
        Entry::new(audit::SECOND_FACTOR_REMOVED, audit::SECOND_FACTOR, id),
    )
    .await?;
    if mail.is_on() {
        let told = FactorChangedEmail {
            to: address.email,
            change: FactorChange::AppRemovedByOperator,
        }
        .render()?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the operator's removal enqueues its email for the account whose factor it \
                      removes (PLATFORM-013)"
        )]
        let job_scope = JobScope::attempt(Some(user_id));
        mail.send(&mut uow, job_scope, told).await?;
    }
    uow.commit().await?;
    Ok(Removal::Removed)
}
