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

/// What the subcommand `remove-second-factor` found (AUTH-072).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecondFactorRemoval {
    /// The account's authenticator app is removed, its sessions ended, and its address told.
    Removed,
    /// No account holds the address, so nothing changed.
    NoAccount,
    /// The account that holds the address has no authenticator app, so nothing changed.
    NoApp,
}

/// Removes the authenticator app and the recovery codes of the account that holds `email`, on
/// `database`, ends every session of its user, and tells the address through `mail`, for the
/// subcommand `remove-second-factor`, which the operator runs once they have checked that the
/// person who asks owns the account (AUTH-072, D135).
pub async fn remove_second_factor(
    database: &Database,
    mail: &Mail,
    email: &platform::types::EmailAddress,
) -> Result<SecondFactorRemoval, Internal> {
    Ok(
        match auth::remove_second_factor(database, mail, email).await? {
            auth::Removal::Removed => SecondFactorRemoval::Removed,
            auth::Removal::NoAccount => SecondFactorRemoval::NoAccount,
            auth::Removal::NoApp => SecondFactorRemoval::NoApp,
        },
    )
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
