//! The subcommands that an operator runs (ARCH-14): [`Operation`], such as
//! `remove-second-factor`, each with what it takes and what it does, as a [`Done`], which
//! `main.rs` takes into its own.

use platform::{db::Database, errors::Internal, mail::Mail};

use crate::modules::auth;
// part: superadmins
use crate::modules::superadmins;
// end part: superadmins
// part: workspace_setup
use crate::modules::workspaces;
// end part: workspace_setup

/// A subcommand that an operator runs against the accounts, on a deployment's server, with what
/// the operator gives it (ARCH-14). Each is one variant here, with its arm in [`Operation::run`],
/// and the binary takes them all into its own subcommands.
#[derive(clap::Subcommand)]
pub enum Operation {
    /// Removes the authenticator app and the recovery codes of the account that holds an
    /// address, ends every session of the account, and tells the address, for a user who lost
    /// both, once the operator has checked that the person who asks owns the account. Exits 1,
    /// changing nothing, when no account holds the address or it has no app.
    RemoveSecondFactor {
        /// The account's email address.
        email: String,
    },
    /// Ends every session of the account that holds an address, or, with `--all`, of every
    /// account. Exits 1, changing nothing, when no account holds the address, or when no session
    /// is there to end.
    EndSessions {
        /// The account's email address.
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        email: Option<String>,
        /// Ends the sessions of every account.
        #[arg(long)]
        all: bool,
    },
    // part: workspace_setup
    /// Creates a workspace, owned by the account that holds an address, and, with email on,
    /// creates that account when no account holds the address, and tells the address. Exits 1,
    /// changing nothing, when an earlier run was given the name, or the account may not own it.
    SetupWorkspace {
        /// The workspace's name.
        name: String,
        /// Its owner's email address.
        email: String,
        /// Its owner's name, for the account that the run creates when none holds the address.
        owner_name: Option<String>,
    },
    // end part: workspace_setup
    // part: superadmins
    /// Makes an account a superadmin of the deployment, ends its being one, or lists the
    /// deployment's superadmins.
    Superadmin {
        /// What to do.
        #[command(subcommand)]
        command: SuperadminCommand,
    },
    // end part: superadmins
}

// part: superadmins
/// What the subcommand `superadmin` does.
#[derive(clap::Subcommand)]
pub enum SuperadminCommand {
    /// Makes the account that holds an address a superadmin. Exits 1, changing nothing, when no
    /// account holds the address, when, with email on, the account hasn't proved it, or when
    /// the account is one already.
    Add {
        /// The account's email address.
        email: String,
    },
    /// Ends the being a superadmin of the account that holds an address. Exits 1, changing
    /// nothing, when no account holds the address, or the account is none.
    Remove {
        /// The account's email address.
        email: String,
    },
    /// Shows the deployment's superadmins, each with the address of their account, the one made
    /// first first.
    List,
}
// end part: superadmins

/// What a subcommand that an operator runs did.
pub struct Done {
    /// What it did, for the subcommand's log, without personal data (OPS-03, OBS-04).
    pub outcome: String,
    // part: superadmins
    /// What it shows the operator who ran it, a line each, which the binary prints to its
    /// standard output, outside the logs, since a line may hold personal data, as the list of
    /// the deployment's superadmins holds their addresses (SUPERADMINS-008).
    pub shown: Vec<String>,
    // end part: superadmins
}

impl Done {
    /// A subcommand that did `outcome`.
    fn of(outcome: impl Into<String>) -> Self {
        Self {
            outcome: outcome.into(),
            // part: superadmins
            shown: Vec::new(),
            // end part: superadmins
        }
    }
}

/// Why an operator's subcommand changed nothing, which its message says without personal data,
/// since the logs hold none (OPS-03, OBS-04).
#[derive(Debug, thiserror::Error)]
pub enum Refused {
    /// Why `remove-second-factor` changed nothing.
    #[error(transparent)]
    RemoveSecondFactor(#[from] RemovalRefused),
    /// Why `end-sessions` changed nothing.
    #[error(transparent)]
    EndSessions(#[from] EndingRefused),
    // part: workspace_setup
    /// Why `setup-workspace` changed nothing.
    #[error(transparent)]
    SetupWorkspace(#[from] SetupRefused),
    // end part: workspace_setup
    // part: superadmins
    /// Why `superadmin` changed nothing.
    #[error(transparent)]
    Superadmin(#[from] SuperadminRefused),
    // end part: superadmins
}

impl Operation {
    /// Runs the subcommand on `database`, telling whom it tells through `mail` while email is
    /// on, and answers what it did, or why it changed nothing.
    pub async fn run(self, database: &Database, mail: &Mail) -> Result<Done, Refused> {
        match self {
            Self::RemoveSecondFactor { email } => {
                let done = remove_second_factor(database, mail, &email).await?;
                Ok(Done::of(done))
            }
            Self::EndSessions { email, .. } => {
                let done = end_sessions(database, email.as_deref()).await?;
                Ok(Done::of(done))
            }
            // part: superadmins
            Self::Superadmin { command } => superadmin(database, mail, command)
                .await
                .map_err(Refused::from),
            // end part: superadmins
            // part: workspace_setup
            Self::SetupWorkspace {
                name,
                email,
                owner_name,
            } => setup_workspace(database, mail, &name, &email, owner_name.as_deref())
                .await
                .map(Done::of)
                .map_err(Refused::from),
            // end part: workspace_setup
        }
    }
}

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
async fn remove_second_factor(
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
             session of it; the account signs in without a second factor, and its address is told",
        ),
        auth::Removal::Removed => Ok(
            "removed the account's authenticator app and recovery codes, and ended every \
             session of it; the account signs in without a second factor, and email is off, so \
             its address isn't told",
        ),
        auth::Removal::NoAccount => Err(RemovalRefused::NoAccount),
        auth::Removal::NoApp => Err(RemovalRefused::NoApp),
    }
}

/// Why the subcommand `end-sessions` changed nothing, which its message says without the address,
/// since the logs hold no personal data (AUTH-094, OBS-04).
#[derive(Debug, thiserror::Error)]
pub enum EndingRefused {
    /// What the operator gave isn't an email address.
    #[error("the address given isn't an email address, so no account holds it: nothing changed")]
    NotAnAddress,
    /// No account holds the address.
    #[error("no account holds the address: nothing changed")]
    NoAccount,
    /// The account that holds the address has no session.
    #[error("the account that holds the address has no session: nothing changed")]
    NoSession,
    /// No account has a session.
    #[error("no account has a session: nothing changed")]
    NoSessions,
    /// The end couldn't run.
    #[error(transparent)]
    Internal(#[from] Internal),
}

/// Ends, on `database`, every session of the account that holds `email`, or of every account
/// when it is `None`, as the subcommand's `--all` asks, for the subcommand `end-sessions`, and
/// answers what it did, with how many sessions it ended, and for every account's, of how many
/// accounts, for the subcommand's log (AUTH-094).
async fn end_sessions(database: &Database, email: Option<&str>) -> Result<String, EndingRefused> {
    let Some(email) = email else {
        return match auth::end_every_session(database).await? {
            auth::Ending::Ended { sessions, users } => Ok(format!(
                "ended every session of every account: sessions {sessions}, accounts {users}"
            )),
            auth::Ending::NoAccount | auth::Ending::NoSession => Err(EndingRefused::NoSessions),
        };
    };
    let Ok(email) = platform::types::EmailAddress::parse(email) else {
        return Err(EndingRefused::NotAnAddress);
    };
    match auth::end_sessions_of(database, &email).await? {
        auth::Ending::Ended { sessions, .. } => Ok(format!(
            "ended every session of the account that holds the address: sessions {sessions}"
        )),
        auth::Ending::NoAccount => Err(EndingRefused::NoAccount),
        auth::Ending::NoSession => Err(EndingRefused::NoSession),
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
async fn setup_workspace(
    database: &Database,
    mail: &Mail,
    name: &str,
    email: &str,
    owner_name: Option<&str>,
) -> Result<String, SetupRefused> {
    let setup = workspaces::Setup::new(name, email, owner_name)?;
    let done: workspaces::SetupDone = workspaces::set_up(database, mail, setup).await??;
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

// part: superadmins
/// Why the subcommand `superadmin` changed nothing, which its message says without the address,
/// since the logs hold no personal data (SUPERADMINS-002, SUPERADMINS-003, OBS-04).
#[derive(Debug, thiserror::Error)]
pub enum SuperadminRefused {
    /// What the operator gave isn't an email address.
    #[error("the address given isn't an email address, so no account holds it: nothing changed")]
    NotAnAddress,
    /// No account holds the address.
    #[error("no account holds the address: nothing changed")]
    NoAccount,
    /// Email is on, and the account that holds the address hasn't proved it.
    #[error(
        "the account that holds the address hasn't proved it, so the address may not be the \
         account's user's: nothing changed; run it again once the account has proved the address"
    )]
    AddressUnproved,
    /// The account is a superadmin already.
    #[error("the account that holds the address is a superadmin already: nothing changed")]
    AlreadySuperadmin,
    /// The account is no superadmin.
    #[error("the account that holds the address is no superadmin: nothing changed")]
    NotSuperadmin,
    /// The change couldn't run.
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<superadmins::Refusal> for SuperadminRefused {
    fn from(refusal: superadmins::Refusal) -> Self {
        match refusal {
            superadmins::Refusal::NoAccount => Self::NoAccount,
            superadmins::Refusal::AddressUnproved => Self::AddressUnproved,
            superadmins::Refusal::AlreadySuperadmin => Self::AlreadySuperadmin,
            superadmins::Refusal::NotSuperadmin => Self::NotSuperadmin,
        }
    }
}

/// Does what `command` says on `database`, for the subcommand `superadmin`, which an operator
/// runs, and answers what it did: makes the account that holds an address a superadmin, or ends
/// its being one, or lists the deployment's superadmins. While `mail` is on, only an account
/// that has proved the address becomes one (SUPERADMINS-001 to SUPERADMINS-003,
/// SUPERADMINS-008, D62).
async fn superadmin(
    database: &Database,
    mail: &Mail,
    command: SuperadminCommand,
) -> Result<Done, SuperadminRefused> {
    let (change, email, done) = match command {
        SuperadminCommand::Add { email } => (
            superadmins::Change::Add,
            email,
            "made the account that holds the address a superadmin; a superadmin's routes serve \
             it once it has an authenticator app",
        ),
        SuperadminCommand::Remove { email } => (
            superadmins::Change::Remove,
            email,
            "the account that holds the address is no superadmin any more",
        ),
        SuperadminCommand::List => return Ok(listed(superadmins::listed(database).await?)),
    };
    let Ok(email) = platform::types::EmailAddress::parse(&email) else {
        return Err(SuperadminRefused::NotAnAddress);
    };
    superadmins::change(database, mail.is_on(), change, &email).await??;
    Ok(Done::of(done))
}

/// What the subcommand's list did, for its log, which holds how many superadmins it listed and
/// no address, and the lines that it shows the operator: each of `superadmins` with the address
/// of their account, their ID and when they became one, or that the deployment has none
/// (SUPERADMINS-008).
fn listed(superadmins: Vec<superadmins::Listed>) -> Done {
    let outcome = format!("listed the deployment's superadmins: {}", superadmins.len());
    let shown = if superadmins.is_empty() {
        vec!["The deployment has no superadmin.".to_owned()]
    } else {
        let title = "The deployment's superadmins, with the ID of each one's user:".to_owned();
        let lines = superadmins.into_iter().map(|superadmin| {
            format!(
                "  {}  {}  since {}",
                superadmin.email.as_str(),
                superadmin.user,
                superadmin.since
            )
        });
        std::iter::once(title).chain(lines).collect()
    };
    Done { outcome, shown }
}
// end part: superadmins
