//! Roles and memberships (ACCESS-001).

use platform::{
    errors::Internal,
    types::{Invalid, Timestamp, UserId, WorkspaceId},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A member's role, from the highest rank to the lowest (ACCESS-001, SEC-08).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Role {
    /// The one member who owns the workspace.
    Owner,
    /// A member who manages the members below them.
    Admin,
    /// A member.
    Member,
}

impl Role {
    /// Every role, from the highest rank to the lowest.
    pub(crate) const ALL: [Self; 3] = [Self::Owner, Self::Admin, Self::Member];

    /// The role's name, as the database's `workspace_role` and the API write it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }

    /// The role named `name`, which a query read or `Authorized<P>` carries.
    pub(crate) fn from_name(name: &str) -> Result<Self, Internal> {
        match name {
            "owner" => Ok(Self::Owner),
            "admin" => Ok(Self::Admin),
            "member" => Ok(Self::Member),
            _ => Err(Internal::new("a role that access doesn't know")),
        }
    }

    /// The role a request names in `value`, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        Self::from_name(value)
            .map_err(|_| Invalid::new("invalid", "A role is owner, admin or member."))
    }

    /// The role's rank: the higher, the more it may do.
    fn rank(self) -> u8 {
        match self {
            Self::Owner => 3,
            Self::Admin => 2,
            Self::Member => 1,
        }
    }

    /// Whether a member at this role may act on a member at `other` (SEC-08).
    pub(crate) fn outranks(self, other: Self) -> bool {
        self.rank() > other.rank()
    }
}

/// One member of one workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Membership {
    /// The workspace.
    pub(crate) workspace_id: WorkspaceId,
    /// The member.
    pub(crate) user_id: UserId,
    /// The member's role.
    pub(crate) role: Role,
    /// When the member joined.
    pub(crate) joined_at: Timestamp,
}

/// Which of a workspace's members a page of them holds (WORKSPACES-004).
#[derive(Clone, Debug, Default)]
pub(crate) struct MemberFilter {
    /// Only the members of this role.
    pub(crate) role: Option<Role>,
    /// Only these users, when a search found them.
    pub(crate) users: Option<Vec<UserId>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_001_a_role_is_owner_admin_or_member_and_ranks_in_that_order() {
        for role in [Role::Owner, Role::Admin, Role::Member] {
            assert_eq!(Role::from_name(role.name()).unwrap(), role);
        }
        assert!(Role::from_name("guest").is_err());
        assert!(Role::Owner.outranks(Role::Admin) && Role::Admin.outranks(Role::Member));
        assert!(!Role::Admin.outranks(Role::Admin) && !Role::Member.outranks(Role::Owner));
    }
}
