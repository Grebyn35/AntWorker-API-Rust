//! The entry points of the binary's subcommands besides `serve` that run the modules' logic:
//! `seed`, which fills a development database through the routes, and `remove-second-factor`,
//! which an operator runs (ARCH-14).

use axum::Router;
use platform::{
    config::Environment,
    db::Database,
    errors::Internal,
    jobs::Jobs,
    mail::{Inbox, Mail},
};

use super::Resources;
use crate::modules::{auth, system};

/// Why the subcommand `remove-second-factor` changed nothing, which its message says without the
/// address, since the logs hold no personal data (AUTH-072, OBS-04).
#[derive(Debug, thiserror::Error)]
pub enum RemovalRefused {
    /// What the operator gave isn't an email address.
    #[error("the address given isn't an email address, so no account holds it: nothing changed")]
    NotAnAddress,
    /// No account holds the address.
    #[error("no account holds the address: nothing changed")]
    NoAccount,
    /// The account that holds the address has no authenticator app.
    #[error("the account that holds the address has no authenticator app: nothing changed")]
    NoApp,
    /// The removal couldn't run.
    #[error(transparent)]
    Internal(#[from] Internal),
}

/// Removes the authenticator app and the recovery codes of the account that holds `email`, on
/// `database`, ends every session of its user, and tells the address through `mail` while email
/// is on, for the subcommand `remove-second-factor`, which the operator runs once they have
/// checked that the person who asks owns the account (AUTH-072, D135).
pub async fn remove_second_factor(
    database: &Database,
    mail: &Mail,
    email: &platform::types::EmailAddress,
) -> Result<(), RemovalRefused> {
    match auth::remove_second_factor(database, mail, email).await? {
        auth::Removal::Removed => Ok(()),
        auth::Removal::NoAccount => Err(RemovalRefused::NoAccount),
        auth::Removal::NoApp => Err(RemovalRefused::NoApp),
    }
}

/// What `seed` created: each account's email address and name, which all share `password`, and
/// each workspace's name and ID (SYSTEM-003).
pub struct SeedReport {
    /// Each account's email address and name, in the order it registered them.
    pub accounts: Vec<(&'static str, &'static str)>,
    /// Every account's password.
    pub password: &'static str,
    /// Each workspace's name and ID.
    pub workspaces: Vec<(&'static str, String)>,
}

/// Why `seed` refused, or stopped (SYSTEM-002, SYSTEM-004).
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct SeedError(system::SystemError);

/// Refuses `seed` unless `environment` is development, before it connects to anything
/// (SYSTEM-002).
pub fn seed_allowed(environment: Environment) -> Result<(), SeedError> {
    system::seed_allowed(environment).map_err(SeedError)
}

/// Fills the database behind `router`, the API as `serve` builds it on `resources`, with demo
/// accounts and workspaces, through its routes, proving each account's address with the code in
/// `inbox`, which `jobs` send while email is on (the system spec's Seed).
pub async fn seed(
    router: Router,
    resources: &Resources,
    jobs: &Jobs,
    inbox: &Inbox,
) -> Result<SeedReport, SeedError> {
    let mailbox = system::Mailbox {
        jobs,
        database: &resources.database,
        clock: &resources.clock,
        inbox,
    };
    let seeded = system::seed(router, mailbox).await.map_err(SeedError)?;
    Ok(SeedReport {
        accounts: seeded
            .accounts
            .iter()
            .map(|account| (account.email, account.name))
            .collect(),
        password: system::PASSWORD,
        workspaces: seeded.workspaces,
    })
}
