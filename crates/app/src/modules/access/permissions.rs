//! The permission matrix, and one type per permission that a route requires (D2, the access
//! spec's "Roles and permissions"). Adding a permission is one type and one row.

use platform::authz::Permission;

use super::model::Role;

/// Listing members, with their names, emails and roles.
pub(crate) struct MembersRead;

impl Permission for MembersRead {
    const NAME: &'static str = "members.read";
}

/// Removing a member of lower rank.
pub(crate) struct MembersRemove;

impl Permission for MembersRemove {
    const NAME: &'static str = "members.remove";
}

// part: invitations
/// Inviting someone at a role below one's own.
pub(crate) struct InvitationsCreate;

impl Permission for InvitationsCreate {
    const NAME: &'static str = "invitations.create";
}
// end part: invitations

/// Each permission, and the roles that hold it (ACCESS-002).
const MATRIX: &[(&str, &[Role])] = &[
    ("workspace.read", &[Role::Owner, Role::Admin, Role::Member]),
    ("members.read", &[Role::Owner, Role::Admin, Role::Member]),
    ("members.remove", &[Role::Owner, Role::Admin]),
    // part: invitations
    ("invitations.create", &[Role::Owner, Role::Admin]),
    // end part: invitations
];

/// Whether `role` holds the permission named `permission`: exactly when the matrix says so
/// (ACCESS-002).
pub(super) fn grants(role: Role, permission: &str) -> bool {
    MATRIX
        .iter()
        .any(|(name, roles)| *name == permission && roles.contains(&role))
}

/// The names of the permissions `role` holds, in the matrix's order (ACCESS-002, ACCESS-007).
pub(super) fn held(role: Role) -> Vec<&'static str> {
    MATRIX
        .iter()
        .filter(|(_, roles)| roles.contains(&role))
        .map(|(name, _)| *name)
        .collect()
}

/// The matrix, as each permission's name with the names of the roles that hold it, which the
/// generated test of every workspace route reads (TEST-08).
pub(crate) fn matrix() -> Vec<(&'static str, Vec<&'static str>)> {
    MATRIX
        .iter()
        .map(|(name, roles)| (*name, roles.iter().map(|role| role.name()).collect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_002_a_role_grants_exactly_the_matrix_s_permissions() {
        let held = |role| {
            [
                "workspace.read",
                "members.read",
                "members.remove",
                // part: invitations
                "invitations.create",
                // end part: invitations
                "files.read",
            ]
            .into_iter()
            .filter(|permission| grants(role, permission))
            .collect::<Vec<_>>()
        };
        let all = vec![
            "workspace.read",
            "members.read",
            "members.remove",
            // part: invitations
            "invitations.create",
            // end part: invitations
        ];
        assert_eq!(held(Role::Owner), all);
        assert_eq!(held(Role::Admin), all);
        assert_eq!(held(Role::Member), ["workspace.read", "members.read"]);
        assert_eq!(MembersRead::NAME, "members.read");
        assert_eq!(MembersRemove::NAME, "members.remove");
    }
}
