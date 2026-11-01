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

/// Each permission, and the roles that hold it (ACCESS-002).
const MATRIX: [(&str, &[Role]); 4] = [
    ("workspace.read", &[Role::Owner, Role::Admin, Role::Member]),
    ("members.read", &[Role::Owner, Role::Admin, Role::Member]),
    ("members.remove", &[Role::Owner, Role::Admin]),
    ("invitations.create", &[Role::Owner, Role::Admin]),
];

/// Whether `role` holds the permission named `permission`: exactly when the matrix says so
/// (ACCESS-002).
pub(super) fn grants(role: Role, permission: &str) -> bool {
    MATRIX
        .iter()
        .any(|(name, roles)| *name == permission && roles.contains(&role))
}

/// The matrix, as each permission's name with the names of the roles that hold it, which the
/// generated test of every workspace route reads (TEST-09).
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
                "invitations.create",
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
            "invitations.create",
        ];
        assert_eq!(held(Role::Owner), all);
        assert_eq!(held(Role::Admin), all);
        assert_eq!(held(Role::Member), ["workspace.read", "members.read"]);
        assert_eq!(MembersRead::NAME, "members.read");
        assert_eq!(MembersRemove::NAME, "members.remove");
    }
}
