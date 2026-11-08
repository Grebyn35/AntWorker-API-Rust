//! The permission matrix, and one type per permission that a route requires (D2, the access
//! spec's "Roles and permissions"). Adding a permission is one type and one row.

use platform::authz::Permission;

use super::model::Role;

/// Reading the workspace, and receiving its events about itself.
pub(crate) struct WorkspaceRead;

impl Permission for WorkspaceRead {
    const NAME: &'static str = "workspace.read";
}

/// Renaming the workspace.
pub(crate) struct WorkspaceUpdate;

impl Permission for WorkspaceUpdate {
    const NAME: &'static str = "workspace.update";
}

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

/// Listing, reading and downloading the workspace's files, and receiving their events.
pub(crate) struct FilesRead;

impl Permission for FilesRead {
    const NAME: &'static str = "files.read";
}

/// Uploading a file.
pub(crate) struct FilesUpload;

impl Permission for FilesUpload {
    const NAME: &'static str = "files.upload";
}

/// Deleting a file.
pub(crate) struct FilesDelete;

impl Permission for FilesDelete {
    const NAME: &'static str = "files.delete";
}

/// Each permission, and the roles that hold it (ACCESS-002).
const MATRIX: &[(&str, &[Role])] = &[
    ("workspace.read", &[Role::Owner, Role::Admin, Role::Member]),
    ("workspace.update", &[Role::Owner, Role::Admin]),
    ("members.read", &[Role::Owner, Role::Admin, Role::Member]),
    ("members.remove", &[Role::Owner, Role::Admin]),
    // part: invitations
    ("invitations.create", &[Role::Owner, Role::Admin]),
    // end part: invitations
    ("files.read", &[Role::Owner, Role::Admin, Role::Member]),
    ("files.upload", &[Role::Owner, Role::Admin, Role::Member]),
    ("files.delete", &[Role::Owner, Role::Admin]),
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

    /// Each row of the access spec's matrix: the permission, and whether `owner`, `admin` and
    /// `member` hold it, as its cells say `yes`.
    #[expect(
        clippy::disallowed_methods,
        reason = "a test may block (CODE-05), and reads the matrix the spec gives"
    )]
    fn spec_matrix() -> Vec<(String, [bool; 3])> {
        let spec = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/spec/access.md"
        ))
        .expect("read the access spec");
        let (_, section) = spec
            .split_once("### Roles and permissions")
            .expect("the spec's matrix");
        section
            .lines()
            .skip_while(|line| !line.starts_with("| Permission |"))
            .skip(2)
            .take_while(|line| line.starts_with('|'))
            .map(|line| {
                let cells: Vec<&str> = line.split('|').map(str::trim).collect();
                let held = |cell: &str| cell.starts_with("yes");
                (
                    cells[1].trim_matches('`').to_owned(),
                    [held(cells[3]), held(cells[4]), held(cells[5])],
                )
            })
            .collect()
    }

    #[test]
    fn access_002_a_role_grants_exactly_the_matrix_s_permissions() {
        let rows = spec_matrix();
        assert!(!rows.is_empty(), "the spec's matrix has no rows");
        for (permission, holders) in &rows {
            for (role, holds) in [Role::Owner, Role::Admin, Role::Member]
                .into_iter()
                .zip(holders)
            {
                assert_eq!(
                    grants(role, permission),
                    *holds,
                    "{permission} for {}",
                    role.name()
                );
            }
        }
        let mut listed: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
        let mut granted: Vec<&str> = MATRIX.iter().map(|(name, _)| *name).collect();
        listed.sort_unstable();
        granted.sort_unstable();
        assert_eq!(granted, listed, "the code's matrix holds other permissions");
        assert!(!grants(Role::Owner, "unlisted.read"));
    }
}
