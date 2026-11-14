//! The tasks on a reference deployment's server, each run from a developer's machine over SSH
//! (OPS-19), as subcommands that `main.rs` flattens into its own: `deploy.rs` runs most of them
//! through the server's deploy/deploy.sh, and `server/` sets a server up and checks it.

use std::process::ExitCode;

use clap::Subcommand;

use crate::{deploy, server};

/// A task on a reference deployment's server.
#[derive(Subcommand)]
pub enum ServerTask {
    /// Deploys the release `version`, or the latest release, to the reference deployment that SSH
    /// reaches at `host` (OPS-14).
    Deploy {
        /// The server, as SSH names it: user@address, or a Host of ~/.ssh/config.
        host: String,
        /// The release's version, such as 1.4.0; the latest by default.
        version: Option<String>,
        /// Restarts a service that runs once, such as Postgres or Caddy, if the release changes
        /// it, which interrupts requests.
        // part: files
        /// The scanner's restart interrupts only uploads.
        // end part: files
        #[arg(long)]
        allow_restart: bool,
    },
    /// Takes a full base backup of the bundled database of the deployment at `host` (OPS-15).
    Backup {
        /// The server, as SSH names it.
        host: String,
    },
    /// Restores the bundled database of the deployment at `host`, as it stood at `moment`, or at
    /// the latest moment its backups hold (OPS-15).
    Restore {
        /// The server, as SSH names it.
        host: String,
        /// The moment: a time with its offset from UTC, as Postgres writes it.
        moment: Option<String>,
        /// Restores although the database's log doesn't reach the bucket, which leaves the
        /// changes that the bucket lacks only in the data that the restore moves aside.
        #[arg(long)]
        allow_unarchived: bool,
    },
    /// Shows the state of the deployment at `host`: which color serves, its containers, its
    /// backups, and the server's free disk and memory (OPS-19).
    Status {
        /// The server, as SSH names it.
        host: String,
    },
    /// Sets up the server that SSH reaches at `host`, as root or a user that may run sudo without
    /// a password: installs Docker, prepares the deployment's user, and writes the settings and
    /// the secrets it asks for (OPS-19). Run again, it changes them.
    #[command(name = "server-setup")]
    ServerSetup {
        /// The server, as SSH names it.
        host: String,
        /// The release whose files the setup follows, such as 1.4.0; the latest by default.
        version: Option<String>,
    },
    /// Checks what a deploy of the release `version`, or of the latest release, to the
    /// deployment at `host` needs, and changes nothing (OPS-19).
    #[command(name = "server-check")]
    ServerCheck {
        /// The server, as SSH names it, as the user that runs the deployment.
        host: String,
        /// The release's version, such as 1.4.0.
        version: Option<String>,
    },
    /// Removes the authenticator app and the recovery codes of the account that holds `email`
    /// on the deployment at `host`, through the subcommand of the image that serves, for a user
    /// who lost both, once the operator has checked that they own the account (AUTH-072, D135).
    #[command(name = "remove-second-factor")]
    RemoveSecondFactor {
        /// The server, as SSH names it.
        host: String,
        /// The account's email address.
        email: String,
    },
    // part: workspace_setup
    /// Creates the workspace `name` on the deployment at `host`, owned by the account that holds
    /// `email`, through the subcommand of the image that serves, which, with email on, creates
    /// that account, named `owner_name`, when no account holds the address (WORKSPACES-023, D65).
    #[command(name = "setup-workspace")]
    SetupWorkspace {
        /// The server, as SSH names it.
        host: String,
        /// The workspace's name.
        name: String,
        /// Its owner's email address.
        email: String,
        /// Its owner's name, for the account that the run creates when none holds the address.
        owner_name: Option<String>,
    },
    // end part: workspace_setup
    /// Shows the last lines of the logs of the deployment's services at `host` (OPS-19).
    Logs {
        /// The server, as SSH names it.
        host: String,
        /// The services, `api` for both colors' replicas, or none for all of them, and
        /// `--follow` to go on showing the lines that follow.
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        arguments: Vec<String>,
    },
}

impl ServerTask {
    /// Runs the task, and exits as it does.
    pub fn run(self) -> ExitCode {
        match self {
            Self::Deploy {
                host,
                version,
                allow_restart,
            } => deploy::deploy(&host, version.as_deref(), allow_restart),
            Self::Backup { host } => deploy::backup(&host),
            Self::Restore {
                host,
                moment,
                allow_unarchived,
            } => deploy::restore(&host, moment.as_deref(), allow_unarchived),
            Self::Status { host } => deploy::status(&host),
            Self::Logs { host, arguments } => deploy::logs(&host, &arguments),
            Self::RemoveSecondFactor { host, email } => deploy::remove_second_factor(&host, &email),
            // part: workspace_setup
            Self::SetupWorkspace {
                host,
                name,
                email,
                owner_name,
            } => deploy::setup_workspace(&host, &name, &email, owner_name.as_deref()),
            // end part: workspace_setup
            Self::ServerSetup { host, version } => server::setup(&host, version.as_deref()),
            Self::ServerCheck { host, version } => server::check(&host, version.as_deref()),
        }
    }
}
