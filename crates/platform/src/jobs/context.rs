//! The proofs of a job's run: the scope a job runs for, and the context of a run, which only the
//! dispatcher and the scheduler build (the platform spec's "The job queue").

use uuid::Uuid;

use crate::{
    authn::{AuditScope, Log},
    types::{UserId, WorkspaceId},
};

/// The user or workspace a job runs for, whose data its handler's queries filter by
/// (PLATFORM-013). A proof builds one, and so do, for no one, `platform`'s scheduler,
/// `Principal::erased_job_scope` for a job that an account's deletion enqueues once the user is
/// erased, and the tests. One comes from no proof: [`JobScope::attempt`], which only a password
/// reset builds, for the account that holds the address it names, and the operator's removal of a
/// second factor, for the account whose factor it removes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobScope {
    user: Option<UserId>,
    workspace: Option<WorkspaceId>,
}

impl JobScope {
    /// The scope of a job that runs for no one, such as scheduled maintenance's.
    pub(crate) fn none() -> Self {
        Self {
            user: None,
            workspace: None,
        }
    }

    /// The scope of no one, for the tests of the job queue, which enqueue jobs that no proof
    /// scopes.
    #[cfg(feature = "test-support")]
    pub fn for_tests() -> Self {
        Self::none()
    }

    /// The scope that a proof gives, which only the proofs of `platform` build.
    pub(crate) fn proven(user: Option<UserId>, workspace: Option<WorkspaceId>) -> Self {
        Self { user, workspace }
    }

    /// The scope of a job that an unauthenticated request, or an operator's subcommand, enqueues
    /// for the account it named, as [`AuditScope::attempt`](crate::authn::AuditScope::attempt)
    /// scopes an entry: that account's user, or no one when no account holds what it named, so
    /// that the request does the same work either way (PLATFORM-013, SEC-05). Only a password reset
    /// and an operator's subcommand call it; clippy bans it elsewhere.
    pub fn attempt(user: Option<UserId>) -> Self {
        Self {
            user,
            workspace: None,
        }
    }

    /// The user the job runs for, if any.
    pub fn user(&self) -> Option<UserId> {
        self.user
    }

    /// The workspace the job runs for, if any.
    pub fn workspace(&self) -> Option<WorkspaceId> {
        self.workspace
    }
}

/// A run of a job, which only the dispatcher and the scheduler build, so only a run opens a unit
/// of work through [`Database::for_job`](crate::db::Database::for_job).
#[derive(Clone, Debug)]
pub struct JobContext {
    pub(super) id: Uuid,
    pub(super) attempt: u32,
    pub(super) scope: JobScope,
}

impl JobContext {
    /// The context of a run of no job, for no one, for the tests of what a run's unit of work
    /// does, such as `Database::for_snapshot_job`'s.
    #[cfg(feature = "test-support")]
    pub fn for_tests() -> Self {
        Self {
            id: Uuid::nil(),
            attempt: 1,
            scope: JobScope::none(),
        }
    }

    /// The queued job's ID, or the claim of a periodic job's run.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Which attempt of its job this run is, from 1.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The scope of an entry that the run records, of a job enqueued for a user: their own log,
    /// with no actor, since no request of theirs acts, or `None` for a job of a workspace or of
    /// no one (DATA-22).
    pub fn audit_scope(&self) -> Option<AuditScope> {
        self.scope
            .user()
            .map(|user| AuditScope::unattributed(Log::User(user)))
    }

    /// The scope the job runs for, which is also the scope of any job the run enqueues.
    pub fn scope(&self) -> JobScope {
        self.scope
    }
}
