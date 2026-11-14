//! The operator's setup of a workspace, which the binary's subcommand `setup-workspace` runs: a
//! workspace, owned by the account that holds its owner's address, which the setup creates when,
//! with email on, no account holds it (the workspaces spec's "The setup subcommand", D65).

use platform::{
    authn::AuditScope,
    db::{Database, UnitOfWork},
    errors::Internal,
    jobs::JobScope,
    mail::Mail,
    types::{EmailAddress, UserId, WorkspaceId},
};

use super::{Founder, found};
use crate::modules::{
    accounts::{self, Creation, DisplayName},
    audit::{self as log, Entry},
    auth,
    workspaces::{audit, emails::SetUpEmail, error::WorkspacesError, model::WorkspaceName, repo},
};

/// What the operator's setup of a workspace takes (WORKSPACES-023).
#[derive(Debug)]
pub(crate) struct Setup {
    name: WorkspaceName,
    email: EmailAddress,
    owner_name: Option<DisplayName>,
}

impl Setup {
    /// The setup of a workspace named `name`, for the owner whose address is `email`, whose
    /// account, when the setup creates it, is named `owner_name`; or why they describe none
    /// (WORKSPACES-028).
    pub(crate) fn new(
        name: &str,
        email: &str,
        owner_name: Option<&str>,
    ) -> Result<Self, SetupRefusal> {
        let name = WorkspaceName::parse(name).map_err(|_| SetupRefusal::NotAName)?;
        let email = EmailAddress::parse(email).map_err(|_| SetupRefusal::NotAnAddress)?;
        let owner_name = owner_name
            .map(DisplayName::parse)
            .transpose()
            .map_err(|_| SetupRefusal::NotAnOwnerName)?;
        Ok(Self {
            name,
            email,
            owner_name,
        })
    }
}

/// Why a setup changed nothing (WORKSPACES-028).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupRefusal {
    /// The workspace's name is none that WORKSPACES-002 allows.
    NotAName,
    /// The owner's address is none that ACCOUNTS-002 allows.
    NotAnAddress,
    /// The owner's name is none that ACCOUNTS-003 allows.
    NotAnOwnerName,
    /// An earlier run was given the name (WORKSPACES-027).
    NameUsed,
    /// With email on, the account that holds the address has an identity, and hasn't proved the
    /// address (WORKSPACES-024).
    AddressUnproved,
    /// With email on, no account holds the address, and the setup was given no name for the
    /// account it would create.
    OwnerNameMissing,
    /// With email on, an account took the address while the setup created its own.
    AddressTaken,
    /// With email off, no account holds the address (WORKSPACES-026).
    NoAccount,
    /// With email off, the account that holds the address has no identity (WORKSPACES-026).
    NoIdentity,
}

/// The account that a setup made the owner of its workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SetupOwner {
    /// The account the setup created, which has no identity (WORKSPACES-024).
    Created,
    /// An account that held the address and has no identity yet, as one that an earlier setup
    /// created has until its owner sets a password.
    WithoutIdentity,
    /// An account that held the address and signs in.
    SignsIn,
}

/// What a setup did (WORKSPACES-023).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SetupDone {
    /// The workspace it created.
    pub(crate) workspace: WorkspaceId,
    /// The account that owns it.
    pub(crate) owner: SetupOwner,
}

/// Creates the workspace that `setup` names, owned by the account that holds its address, which
/// it creates when, with email on, no account holds it, in one unit of work on `database`, which
/// records what it creates with no actor, keeps the name against a later setup, and enqueues,
/// through `mail`, the email that tells the address; or reports why it changed nothing
/// (WORKSPACES-023 to WORKSPACES-028). It publishes no event: the membership's signal closes the
/// owner's streams, and their clients read their workspaces again.
pub(crate) async fn set_up(
    database: &Database,
    mail: &Mail,
    setup: Setup,
) -> Result<Result<SetupDone, SetupRefusal>, Internal> {
    run(database, mail, setup)
        .await
        .map_err(|error| match error {
            WorkspacesError::Internal(error) => error,
            other => Internal::new(other),
        })
}

/// [`set_up`], whose steps report the module's errors, which are only ever internal here.
async fn run(
    database: &Database,
    mail: &Mail,
    setup: Setup,
) -> Result<Result<SetupDone, SetupRefusal>, WorkspacesError> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the operator's subcommand acts for no request and no job (ARCH-14)"
    )]
    let mut uow = database.for_operator().await?;
    let (user, owner) = match owner(&mut uow, mail.is_on(), &setup).await? {
        Ok(owner) => owner,
        Err(refusal) => return Ok(Err(refusal)),
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the operator's setup records the workspace it creates with no actor (the \
                  workspaces spec)"
    )]
    let founder = Founder {
        owner: user,
        scope: AuditScope::setup,
    };
    let workspace = found(&mut uow, &setup.name, founder).await?.workspace.id;
    if !repo::setup::record(&mut uow, workspace, &setup.name).await? {
        return Ok(Err(SetupRefusal::NameUsed));
    }
    if mail.is_on() {
        let told = SetUpEmail {
            to: setup.email,
            workspace: setup.name,
            sets_password: owner != SetupOwner::SignsIn,
        }
        .render()?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the operator's setup enqueues its email for the account that owns the \
                      workspace (PLATFORM-013)"
        )]
        let scope = JobScope::attempt(Some(user));
        mail.send(&mut uow, scope, told).await?;
    }
    uow.commit().await?;
    Ok(Ok(SetupDone { workspace, owner }))
}

/// The account that is to own the workspace of `setup`: the one that holds its address, whose
/// row `uow` then holds, when it may own it, or, with email on, one that it creates, and records
/// in the new account's log (WORKSPACES-024, WORKSPACES-026).
async fn owner(
    uow: &mut UnitOfWork,
    email_on: bool,
    setup: &Setup,
) -> Result<Result<(UserId, SetupOwner), SetupRefusal>, WorkspacesError> {
    // The user's row first, as the user's own changes and a deletion of the account lock it
    // (DATA-20). An account that a deletion took, or that changed its address, while the lock
    // waited holds the address no more.
    if let Some((user, address)) = accounts::holder_for_update(uow, &setup.email).await? {
        let identity = auth::has_identity(uow, user).await?;
        return Ok(match (email_on, address.verified, identity) {
            (true, true, true) | (false, _, true) => Ok((user, SetupOwner::SignsIn)),
            (true, _, false) => Ok((user, SetupOwner::WithoutIdentity)),
            (true, false, true) => Err(SetupRefusal::AddressUnproved),
            (false, _, false) => Err(SetupRefusal::NoIdentity),
        });
    }
    if !email_on {
        return Ok(Err(SetupRefusal::NoAccount));
    }
    let Some(name) = &setup.owner_name else {
        return Ok(Err(SetupRefusal::OwnerNameMissing));
    };
    let Creation::Created(user) = accounts::create(uow, &setup.email, name).await? else {
        return Ok(Err(SetupRefusal::AddressTaken));
    };
    let entry = Entry::new(audit::USER_CREATED, audit::USER, user.uuid());
    log::record(uow, AuditScope::attempt(user), entry).await?;
    Ok(Ok((user, SetupOwner::Created)))
}
