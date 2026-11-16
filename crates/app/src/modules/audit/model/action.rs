//! An audit action, as a module declares it in its `audit.rs`, and the kinds of resource that
//! entries happen to (the audit spec's "The action catalog").

use std::{fmt, marker::PhantomData};

use platform::{authn::Log, authz::Permission, errors::Internal};
use serde::Serialize;
use utoipa::ToSchema;

/// What happened, as `<resource>.<past-tense verb>`, such as `user.registered`, as the action
/// catalog lists it: a sentence that tells a client what it means, the kinds of resource it
/// happens to, the logs that record it, with the permission that reading it in a workspace's log
/// takes, if any, and `D`, the type of the details each of its entries holds. Each module
/// declares its own in its `audit.rs`, the one place clippy lets build one, and lists them there
/// for the catalog (AUDIT-012, AUDIT-014).
pub(crate) struct Action<D = NoDetails> {
    name: &'static str,
    description: &'static str,
    resources: &'static [Resource],
    own_log: bool,
    workspace_log: Option<Readers>,
    details: PhantomData<fn() -> D>,
}

/// The readers of a workspace's log who read an action's entries (AUDIT-007).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Readers {
    /// Whoever reads the log, as with a refusal.
    Every,
    /// A reader whose role holds the permission of this name.
    Holding(&'static str),
}

impl<D> Action<D> {
    /// The action `name`, which `description`, a sentence, tells a client the meaning of, and
    /// which happens to a resource of one of the kinds `resources`. It is recorded in no log
    /// until one of the methods below names one.
    pub(crate) const fn new(
        name: &'static str,
        description: &'static str,
        resources: &'static [Resource],
    ) -> Self {
        Self {
            name,
            description,
            resources,
            own_log: false,
            workspace_log: None,
            details: PhantomData,
        }
    }

    /// The action, which a user's own log records too.
    pub(crate) const fn in_own_log(self) -> Self {
        Self {
            own_log: true,
            ..self
        }
    }

    /// The action, which a workspace's log records too, and shows only to a reader whose role
    /// holds `P`: the permission of the workspace event that its change publishes, or
    /// `workspace.read` for a change that publishes none (the audit spec's "Reading a
    /// workspace's log").
    pub(crate) const fn in_workspace_log<P: Permission>(self) -> Self {
        Self {
            workspace_log: Some(Readers::Holding(P::NAME)),
            ..self
        }
    }

    /// The action, which a workspace's log records too, and shows to whoever reads it, as a
    /// refusal (AUDIT-004, AUDIT-007).
    pub(crate) const fn in_workspace_log_for_every_reader(self) -> Self {
        Self {
            workspace_log: Some(Readers::Every),
            ..self
        }
    }

    /// The action's name.
    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    /// The sentence that tells a client what the action means.
    pub(crate) fn description(&self) -> &'static str {
        self.description
    }

    /// The kinds of resource that it happens to.
    pub(crate) fn resources(&self) -> &'static [Resource] {
        self.resources
    }

    /// Whether a user's own log records it.
    pub(crate) fn in_own_logs(&self) -> bool {
        self.own_log
    }

    /// Whether a workspace's log records it.
    pub(crate) fn in_workspace_logs(&self) -> bool {
        self.workspace_log.is_some()
    }

    /// The permission that reading it in a workspace's log takes beside `audit.read`, if any.
    pub(crate) fn permission(&self) -> Option<&'static str> {
        match self.workspace_log {
            Some(Readers::Holding(permission)) => Some(permission),
            Some(Readers::Every) | None => None,
        }
    }

    /// Nothing, when the action declares `log`'s kind of log and `resource`; otherwise the
    /// internal error that keeps an entry it doesn't declare out of every log (AUDIT-013).
    pub(super) fn admits(&self, log: Log, resource: Resource) -> Result<(), Internal> {
        let in_log = match log {
            Log::User(_) => self.own_log,
            Log::Workspace(_) => self.workspace_log.is_some(),
        };
        if !in_log {
            return Err(Internal::new(format!(
                "the audit action {} declares no log of this kind",
                self.name
            )));
        }
        if !self.resources.contains(&resource) {
            return Err(Internal::new(format!(
                "the audit action {} declares no resource {}",
                self.name, resource.0
            )));
        }
        Ok(())
    }
}

impl<D> Clone for Action<D> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<D> Copy for Action<D> {}

impl<D> fmt::Debug for Action<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Action")
            .field("name", &self.name)
            .field("resources", &self.resources)
            .field("own_log", &self.own_log)
            .field("workspace_log", &self.workspace_log)
            .finish_non_exhaustive()
    }
}

/// The details of an action that records none: an empty object.
#[derive(Clone, Copy, Debug, Default, Serialize, ToSchema)]
pub(crate) struct NoDetails {}

// part: superadmins
/// How a caller acted, other than as a member of a workspace: as a superadmin, whose route a
/// refusal names, or whose creation or deletion of a workspace the superadmin's own log records
/// (SUPERADMINS-004, WORKSPACES-030, WORKSPACES-031).
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Access {
    /// As a superadmin.
    Superadmin,
}
// end part: superadmins

/// A kind of resource that entries happen to, such as `user` or `session`. Each module declares
/// its own in its `audit.rs` (the audit spec), the one place clippy lets build one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Resource(&'static str);

impl Resource {
    /// The kind of resource `name`.
    pub(crate) const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The kind's name.
    pub(crate) fn name(self) -> &'static str {
        self.0
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "the tests declare actions and resources of their own, as an audit.rs does"
)]
mod tests {
    use platform::types::{UserId, WorkspaceId};

    use super::*;
    use crate::modules::access::MembersRead;

    const WORKSPACE: Resource = Resource::new("workspace");
    const USER: Resource = Resource::new("user");

    #[test]
    fn audit_013_an_action_admits_only_the_logs_and_the_resources_it_declares() {
        let user = Log::User(UserId::from_uuid(uuid::Uuid::nil()));
        let workspace = Log::Workspace(WorkspaceId::from_uuid(uuid::Uuid::nil()));
        let own: Action = Action::new("user.renamed", "Renamed.", &[USER]).in_own_log();
        assert!(own.admits(user, USER).is_ok());
        assert!(own.admits(workspace, USER).is_err());
        assert!(own.admits(user, WORKSPACE).is_err());
        let shared: Action = Action::new("member.removed", "Removed.", &[WORKSPACE])
            .in_workspace_log::<MembersRead>();
        assert!(shared.admits(workspace, WORKSPACE).is_ok());
        assert!(shared.admits(user, WORKSPACE).is_err());
        assert_eq!(shared.permission(), Some("members.read"));
        let both: Action = Action::new("access.denied", "Refused.", &[WORKSPACE, USER])
            .in_own_log()
            .in_workspace_log_for_every_reader();
        for (log, resource) in [(user, WORKSPACE), (user, USER), (workspace, WORKSPACE)] {
            assert!(both.admits(log, resource).is_ok());
        }
        assert_eq!(both.permission(), None);
        let nowhere: Action = Action::new("thing.done", "Done.", &[WORKSPACE]);
        assert!(nowhere.admits(user, WORKSPACE).is_err());
        assert!(nowhere.admits(workspace, WORKSPACE).is_err());
    }
}
