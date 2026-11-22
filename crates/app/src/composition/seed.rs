//! The entry points of `seed`, the binary's subcommand that fills a development database
//! (ARCH-14, the system spec's Seed).

use axum::Router;
use platform::{config::Environment, jobs::Jobs, mail::Inbox};

use super::Resources;
use crate::modules::system;
// part: superadmins
use crate::modules::superadmins;
// end part: superadmins
// part: workspace_setup
use crate::modules::workspaces;
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
    // part: superadmins
    /// The address of the account that it made a superadmin, and the secret of the account's
    /// authenticator app, in base32, whose codes a sign-in to the account takes (SYSTEM-010).
    pub superadmin: (&'static str, platform::types::Secret<String>),
    // end part: superadmins
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
// part: superadmins
/// It hands the seed the operator's making of a superadmin, on the same `resources`, for the one
/// account that the seed makes a superadmin as an operator does (SYSTEM-010).
// end part: superadmins
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
    let set_up: system::SetUp<'_> = &set_up;
    // end part: workspace_setup
    // part: superadmins
    let make_superadmin = |email: &'static str| -> system::MakingSuperadmin<'_> {
        Box::pin(make_superadmin_for_seed(resources, email))
    };
    let make_superadmin: system::MakeSuperadmin<'_> = &make_superadmin;
    // end part: superadmins
    let seed = system::Seed {
        router,
        mailbox,
        // part: workspace_setup
        set_up,
        // end part: workspace_setup
        // part: superadmins
        make_superadmin,
        // end part: superadmins
    };
    let seeded: system::Seeded = system::seed(seed).await.map_err(SeedError)?;
    Ok(SeedReport {
        accounts: seeded
            .accounts
            .iter()
            .map(|account: &system::Account| (account.email, account.name))
            .collect(),
        password: system::PASSWORD,
        workspaces: seeded.workspaces,
        // part: superadmins
        superadmin: seeded.superadmin,
        // end part: superadmins
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
) -> Result<String, platform::errors::Internal> {
    let refused = |refusal: workspaces::SetupRefusal| {
        platform::errors::Internal::new(format!(
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

// part: superadmins
/// Makes the account that holds `email`, which the seed registered, a superadmin, as an
/// operator's run of `superadmin add` does, on the seed's database, which takes an account that
/// has proved its address while the seed's mail is on (SYSTEM-010).
async fn make_superadmin_for_seed(
    resources: &Resources,
    email: &str,
) -> Result<(), platform::errors::Internal> {
    let email_on = resources.mail.is_on();
    let email = platform::types::EmailAddress::parse(email).map_err(|_| {
        platform::errors::Internal::new("the seed's superadmin has no email address")
    })?;
    superadmins::change(
        &resources.database,
        email_on,
        superadmins::Change::Add,
        &email,
    )
    .await?
    .map_err(|refusal| {
        platform::errors::Internal::new(format!(
            "the seed's making of a superadmin changed nothing: {refusal:?}"
        ))
    })
}
// end part: superadmins
