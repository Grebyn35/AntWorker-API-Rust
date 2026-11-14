//! The entry points of the binary's subcommands besides `serve` that run the modules' logic:
//! `seed`, which fills a development database, and those that an operator runs, such as
//! `remove-second-factor` (ARCH-14).

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
// part: workspace_setup
use crate::modules::workspaces;
// end part: workspace_setup

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
/// checked that the person who asks owns the account, and answers what it did, for the
/// subcommand's log (AUTH-072, D135).
pub async fn remove_second_factor(
    database: &Database,
    mail: &Mail,
    email: &str,
) -> Result<&'static str, RemovalRefused> {
    let Ok(email) = platform::types::EmailAddress::parse(email) else {
        return Err(RemovalRefused::NotAnAddress);
    };
    match auth::remove_second_factor(database, mail, &email).await? {
        auth::Removal::Removed if mail.is_on() => Ok(
            "removed the account's authenticator app and recovery codes, and ended every \
             session of it; the account signs in with its password alone, and its address is told",
        ),
        auth::Removal::Removed => Ok(
            "removed the account's authenticator app and recovery codes, and ended every \
             session of it; the account signs in with its password alone, and email is off, so \
             its address isn't told",
        ),
        auth::Removal::NoAccount => Err(RemovalRefused::NoAccount),
        auth::Removal::NoApp => Err(RemovalRefused::NoApp),
    }
}

// part: workspace_setup
/// Why the subcommand `setup-workspace` changed nothing, which its message says without the
/// workspace's name or the address, since the logs hold no personal data (WORKSPACES-028,
/// OBS-04).
#[derive(Debug, thiserror::Error)]
pub enum SetupRefused {
    /// What the operator gave as the workspace's name is none.
    #[error("the name given isn't a workspace's name, of 1 to 255 characters: nothing changed")]
    NotAName,
    /// What the operator gave as the owner's address isn't an email address.
    #[error("the address given isn't an email address: nothing changed")]
    NotAnAddress,
    /// What the operator gave as the owner's name is none.
    #[error("the owner's name given isn't a name, of 1 to 100 characters: nothing changed")]
    NotAnOwnerName,
    /// An earlier run was given the name.
    #[error("an earlier run was given the name, and set its workspace up: nothing changed")]
    NameUsed,
    /// The account that holds the address has an identity, and hasn't proved the address.
    #[error(
        "the account that holds the address hasn't proved it, so it may not be its owner's: \
         nothing changed; run it again once the account has proved the address"
    )]
    AddressUnproved,
    /// No account holds the address, and the run was given no name for the one it would create.
    #[error(
        "no account holds the address, and the account to create needs its owner's name, which \
         wasn't given: nothing changed"
    )]
    OwnerNameMissing,
    /// An account took the address while the run created its own.
    #[error("an account took the address while the subcommand ran: nothing changed; run it again")]
    AddressTaken,
    /// Email is off, and no account holds the address.
    #[error(
        "no account holds the address, and with email off the subcommand creates none, since \
         its owner could set no password: nothing changed; run it again once the owner has \
         registered"
    )]
    NoAccount,
    /// Email is off, and the account that holds the address has no identity.
    #[error(
        "the account that holds the address has no password and no sign-in provider, and with \
         email off its owner can set no password: nothing changed"
    )]
    NoIdentity,
    /// The setup couldn't run.
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<workspaces::SetupRefusal> for SetupRefused {
    fn from(refusal: workspaces::SetupRefusal) -> Self {
        match refusal {
            workspaces::SetupRefusal::NotAName => Self::NotAName,
            workspaces::SetupRefusal::NotAnAddress => Self::NotAnAddress,
            workspaces::SetupRefusal::NotAnOwnerName => Self::NotAnOwnerName,
            workspaces::SetupRefusal::NameUsed => Self::NameUsed,
            workspaces::SetupRefusal::AddressUnproved => Self::AddressUnproved,
            workspaces::SetupRefusal::OwnerNameMissing => Self::OwnerNameMissing,
            workspaces::SetupRefusal::AddressTaken => Self::AddressTaken,
            workspaces::SetupRefusal::NoAccount => Self::NoAccount,
            workspaces::SetupRefusal::NoIdentity => Self::NoIdentity,
        }
    }
}

/// Creates the workspace named `name` on `database`, owned by the account that holds `email`,
/// which it creates, named `owner_name`, when, with email on, no account holds the address, and
/// tells the address through `mail` while email is on, for the subcommand `setup-workspace`,
/// which an operator runs, and answers what it did, with the workspace's ID, for the
/// subcommand's log (WORKSPACES-023 to WORKSPACES-028, D65).
pub async fn setup_workspace(
    database: &Database,
    mail: &Mail,
    name: &str,
    email: &str,
    owner_name: Option<&str>,
) -> Result<String, SetupRefused> {
    let setup = workspaces::Setup::new(name, email, owner_name)?;
    let done = workspaces::set_up(database, mail, setup).await??;
    let workspace = done.workspace;
    Ok(match (done.owner, mail.is_on()) {
        (workspaces::SetupOwner::Created, _) => format!(
            "set up the workspace {workspace}, and created its owner's account, which has no \
             password; its address is told how to set one"
        ),
        (workspaces::SetupOwner::WithoutIdentity, _) => format!(
            "set up the workspace {workspace}, whose owner is the account that holds the \
             address, which has no password yet; its address is told how to set one"
        ),
        (workspaces::SetupOwner::SignsIn, true) => format!(
            "set up the workspace {workspace}, whose owner is the account that holds the \
             address, which is told"
        ),
        (workspaces::SetupOwner::SignsIn, false) => format!(
            "set up the workspace {workspace}, whose owner is the account that holds the \
             address; email is off, so its address isn't told"
        ),
    })
}
// end part: workspace_setup

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
/// accounts and workspaces, as a client of its routes, proving each account's address with the
/// code in `inbox`, which `jobs` send while email is on (the system spec's Seed).
// part: workspace_setup
/// It hands the seed the operator's setup of a workspace, on the same `resources`, for the one
/// workspace that the seed sets up as an operator does (SYSTEM-009).
// end part: workspace_setup
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
    // part: workspace_setup
    let set_up = |name: &'static str, email: &'static str| -> system::SettingUp<'_> {
        Box::pin(set_up_for_seed(resources, name, email))
    };
    // end part: workspace_setup
    let seed = system::Seed {
        router,
        mailbox,
        // part: workspace_setup
        set_up: &set_up,
        // end part: workspace_setup
    };
    let seeded = system::seed(seed).await.map_err(SeedError)?;
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

// part: workspace_setup
/// Sets up the workspace named `name` for the account that holds `email`, which the seed
/// registered, as an operator's run of `setup-workspace` does, on the seed's database and with
/// its mail, and answers the workspace's ID (SYSTEM-009).
async fn set_up_for_seed(
    resources: &Resources,
    name: &str,
    email: &str,
) -> Result<String, Internal> {
    let refused = |refusal: workspaces::SetupRefusal| {
        Internal::new(format!(
            "the seed's setup of a workspace set nothing up: {refusal:?}"
        ))
    };
    let setup = workspaces::Setup::new(name, email, None).map_err(refused)?;
    let done = workspaces::set_up(&resources.database, &resources.mail, setup)
        .await?
        .map_err(refused)?;
    Ok(done.workspace.to_string())
}
// end part: workspace_setup
