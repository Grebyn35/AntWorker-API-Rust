//! The workspaces a user belongs to, as the hub reads them for its streams, and the transactions
//! that had committed when an event did, by which a stream receives only the events that
//! committed with its caller's membership or after it (SEC-19).

use std::collections::BTreeMap;

use crate::types::WorkspaceId;

/// The workspaces a user belongs to, each with the names of the permissions their role holds
/// there and the transaction that added them, which `AccessPolicy::memberships` reads for the
/// hub, so a stream receives only the workspace events its caller may, and of those only the ones
/// that committed with their membership or after it (SEC-19).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Memberships {
    workspaces: BTreeMap<WorkspaceId, Membership>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Membership {
    permissions: Vec<&'static str>,
    joined: u64,
}

impl Memberships {
    /// Records that the user belongs to `workspace`, with a role that holds `permissions`, since
    /// the transaction `joined` committed: its ID, as `pg_current_xact_id()` gave it.
    pub fn insert(&mut self, workspace: WorkspaceId, joined: u64, permissions: &[&'static str]) {
        let mut permissions = permissions.to_vec();
        permissions.sort_unstable();
        self.workspaces.insert(
            workspace,
            Membership {
                permissions,
                joined,
            },
        );
    }

    /// The workspaces the user belongs to.
    pub(crate) fn workspaces(&self) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.workspaces.keys().copied()
    }

    /// Whether an event of `workspace` that requires `permission`, and committed when `committed`
    /// had, reaches the user: their role there holds the permission, and their membership had
    /// committed before the event did.
    pub(crate) fn admits(
        &self,
        workspace: WorkspaceId,
        permission: &str,
        committed: &Committed,
    ) -> bool {
        self.workspaces.get(&workspace).is_some_and(|membership| {
            membership.permissions.contains(&permission) && committed.includes(membership.joined)
        })
    }

    /// One row per workspace and permission the user holds there: its workspace, the
    /// permission, and the transaction that added the membership, as text for `xid8`, which the
    /// replay's query joins on.
    pub(crate) fn rows(&self) -> (Vec<uuid::Uuid>, Vec<String>, Vec<String>) {
        let mut rows = (Vec::new(), Vec::new(), Vec::new());
        for (workspace, membership) in &self.workspaces {
            for permission in &membership.permissions {
                rows.0.push(workspace.uuid());
                rows.1.push((*permission).to_owned());
                rows.2.push(membership.joined.to_string());
            }
        }
        rows
    }
}

/// The transactions a `pg_snapshot` holds as completed: those below `xmin`, and those below `xmax`
/// that weren't running, as Postgres's `pg_visible_in_snapshot` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    xmin: u64,
    xmax: u64,
    running: Vec<u64>,
}

impl Snapshot {
    /// The snapshot whose text form, `xmin:xmax:xip,...`, is `text`, or `None` for text that
    /// isn't one.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let (Some(xmin), Some(xmax), Some(running), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let running = running
            .split(',')
            .filter(|id| !id.is_empty())
            .map(str::parse)
            .collect::<Result<Vec<u64>, _>>()
            .ok()?;
        Some(Self {
            xmin: xmin.parse().ok()?,
            xmax: xmax.parse().ok()?,
            running,
        })
    }

    /// Whether the transaction `id` had completed when the snapshot was taken.
    pub(crate) fn includes(&self, id: u64) -> bool {
        id < self.xmin || (id < self.xmax && !self.running.contains(&id))
    }
}

/// The transactions that had committed when an event did: those of the snapshot its commit took,
/// and the event's own, which a snapshot leaves out, but which commits with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Committed {
    own: u64,
    snapshot: Snapshot,
}

impl Committed {
    /// The transactions of the event whose own transaction is `own`, and whose commit took
    /// `snapshot`, a `pg_snapshot` in its text form, or `None` for text that isn't one.
    pub(crate) fn parse(own: &str, snapshot: &str) -> Option<Self> {
        Some(Self {
            own: own.parse().ok()?,
            snapshot: Snapshot::parse(snapshot)?,
        })
    }

    /// Whether the transaction `id` had committed, or is the event's own.
    fn includes(&self, id: u64) -> bool {
        id == self.own || self.snapshot.includes(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_029_a_workspace_event_reaches_a_member_who_joined_before_it_or_in_its_transaction()
    {
        let workspace = WorkspaceId::from_uuid(uuid::Uuid::from_u128(1));
        // The event's transaction is 110, whose commit's snapshot holds as completed those below
        // 100, and those from 100 to 109 but 104, which was still running.
        let committed = Committed::parse("110", "100:110:104").unwrap();
        let reaches = |joined: u64| {
            let mut memberships = Memberships::default();
            memberships.insert(workspace, joined, &["members.read"]);
            memberships.admits(workspace, "members.read", &committed)
        };

        let reached: Vec<u64> = [99, 104, 105, 110, 111]
            .into_iter()
            .filter(|joined| reaches(*joined))
            .collect();

        assert_eq!(reached, [99, 105, 110]);
    }
}
