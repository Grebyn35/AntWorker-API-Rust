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
        /// Restarts a service that runs once, such as Postgres or Caddy, if the deploy changes
        /// it, through the release, a setting, a secret or a file it reads, which interrupts
        /// requests; and replaces the network of Caddy and the replicas if `PROXY_SUBNET` names
        /// another subnet than the network's, which stops Caddy and every replica, so that no
        /// request is served until the release's replicas serve.
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
        /// Restores from the backups as their bucket held them at this time, whatever it holds
        /// now, as after someone who holds the server deleted them (OPS-22): a time with its
        /// offset from UTC, as Postgres writes it, in one word or as its date and its time. Such
        /// a restore checks nothing of the database it replaces, so `--allow-unarchived` has
        /// nothing to allow beside it.
        #[arg(long, value_name = "TIME", num_args = 1..=2, conflicts_with = "allow_unarchived")]
        bucket_as_of: Vec<String>,
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
    /// deployment at `host` needs, and changes nothing of it but an object that it writes to each
    /// bucket and deletes (OPS-19, OPS-22).
    #[command(name = "server-check")]
    ServerCheck {
        /// The server, as SSH names it, as the user that runs the deployment.
        host: String,
        /// The release's version, such as 1.4.0.
        version: Option<String>,
    },
    /// Writes a new key in place of one that a deploy generated on the deployment at `host`,
    /// which the next deploy gives the services that read it (OPS-19).
    #[command(name = "replace-key")]
    ReplaceKey {
        /// The server, as SSH names it.
        host: String,
        /// The key's file: `hmac_key`, the key of the API's keyed hashes, or
        /// `grafana_secret_key`, the one under which Grafana encrypts what it stores.
        key: String,
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
    /// Ends every session of the account that holds an address on the deployment at `host`, or,
    /// with `--all` in the address's place, of every account, through the subcommand of the
    /// image that serves (AUTH-094).
    #[command(name = "end-sessions")]
    EndSessions {
        /// The server, as SSH names it.
        host: String,
        /// The account's email address, or `--all`, for every account.
        #[arg(allow_hyphen_values = true)]
        whose: String,
    },
    // part: files
    /// Writes again each object of the files bucket of the deployment at `host` whose version at
    /// `moment` isn't its current one, as one deleted or replaced since, from the version that
    /// the bucket kept, through the subcommand of the image that serves (FILES-028, OPS-22).
    #[command(name = "restore-files")]
    RestoreFiles {
        /// The server, as SSH names it.
        host: String,
        /// The moment: a time with its offset from UTC, as Postgres writes it.
        moment: String,
    },
    // end part: files
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
    // part: superadmins
    /// Makes an account a superadmin of the deployment at `host`, ends its being one, or lists
    /// the deployment's superadmins, through the subcommand of the image that serves
    /// (SUPERADMINS-001, SUPERADMINS-008, D62).
    Superadmin {
        /// The server, as SSH names it.
        host: String,
        /// What to do.
        #[command(subcommand)]
        command: SuperadminTask,
    },
    // end part: superadmins
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

// part: superadmins
/// What `just superadmin` does on a deployment (SUPERADMINS-001, SUPERADMINS-003,
/// SUPERADMINS-008).
#[derive(Subcommand)]
pub enum SuperadminTask {
    /// Makes the account that holds `email` a superadmin.
    Add {
        /// The account's email address.
        email: String,
    },
    /// Ends the being a superadmin of the account that holds `email`.
    Remove {
        /// The account's email address.
        email: String,
    },
    /// Shows the deployment's superadmins, each with the address of their account.
    List,
}

impl SuperadminTask {
    /// What the API's subcommand `superadmin` takes for the task.
    fn arguments(&self) -> Vec<&str> {
        match self {
            Self::Add { email } => vec!["add", email],
            Self::Remove { email } => vec!["remove", email],
            Self::List => vec!["list"],
        }
    }
}
// end part: superadmins

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
                bucket_as_of,
            } => {
                let held = (!bucket_as_of.is_empty()).then(|| bucket_as_of.join(" "));
                deploy::restore(&host, moment.as_deref(), allow_unarchived, held.as_deref())
            }
            Self::Status { host } => deploy::status(&host),
            Self::Logs { host, arguments } => deploy::logs(&host, &arguments),
            Self::ReplaceKey { host, key } => deploy::replace_key(&host, &key),
            Self::RemoveSecondFactor { host, email } => deploy::remove_second_factor(&host, &email),
            Self::EndSessions { host, whose } => deploy::end_sessions(&host, &whose),
            // part: files
            Self::RestoreFiles { host, moment } => deploy::restore_files(&host, &moment),
            // end part: files
            // part: workspace_setup
            Self::SetupWorkspace {
                host,
                name,
                email,
                owner_name,
            } => deploy::setup_workspace(&host, &name, &email, owner_name.as_deref()),
            // end part: workspace_setup
            // part: superadmins
            Self::Superadmin { host, command } => deploy::superadmin(&host, &command.arguments()),
            // end part: superadmins
            Self::ServerSetup { host, version } => server::setup(&host, version.as_deref()),
            Self::ServerCheck { host, version } => server::check(&host, version.as_deref()),
        }
    }
}
