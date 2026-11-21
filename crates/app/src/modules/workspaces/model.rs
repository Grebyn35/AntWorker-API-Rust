//! The workspace's domain types (D25), and what its routes answer.

use std::borrow::Cow;

use platform::types::{
    EmailAddress, ID_CHARS, Invalid, Timestamp, UserId, Version, WorkspaceId, one_line, read_id,
};
use serde::Serialize;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{KnownFormat, ObjectBuilder, RefOr, Schema, SchemaFormat, Type},
};

use crate::modules::{access::Role, accounts::DisplayName};

/// The most characters a workspace name has (WORKSPACES-002).
const NAME_MAX_CHARS: usize = 255;

/// A workspace's name: trimmed, with each run of whitespace inside collapsed to one space, in
/// Unicode Normalization Form C, of 1 to 255 characters, with no control character
/// (WORKSPACES-002). A name can hold a person's name, so `Debug` doesn't show it (OBS-04). `Deserialize` only reads back a stored
/// result that an idempotency key replays (PLATFORM-036), which held a parsed name.
#[derive(Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub(crate) struct WorkspaceName(String);

impl std::fmt::Debug for WorkspaceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkspaceName([redacted])")
    }
}

impl WorkspaceName {
    /// The name in `value`, normalized, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        one_line(
            value,
            NAME_MAX_CHARS,
            "Enter a name.",
            "A workspace name has at most 255 characters.",
        )
        .map(Self)
    }

    /// A name the database holds, which was normalized when it was stored.
    pub(super) fn from_stored(name: String) -> Self {
        Self(name)
    }

    /// The normalized name.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialSchema for WorkspaceName {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(1))
            .max_length(Some(NAME_MAX_CHARS))
            .into()
    }
}

impl ToSchema for WorkspaceName {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("WorkspaceName")
    }
}

// part: workspace_creation
/// A workspace that a user creates (WORKSPACES-001).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewWorkspace {
    /// Its name.
    pub(crate) name: WorkspaceName,
}
// end part: workspace_creation

/// A workspace, as one of its members reads it, with their role and the permissions it holds
/// (WORKSPACES-007).
// `Deserialize` only reads back the stored result of a creation of one, which an idempotency
// key replays (PLATFORM-036), through `Stored`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize, ToSchema)]
#[serde(from = "Stored")]
pub(crate) struct Workspace {
    /// The workspace's ID.
    pub(crate) id: WorkspaceId,
    /// Its name.
    pub(crate) name: WorkspaceName,
    /// The caller's role in it.
    pub(crate) role: Role,
    /// The names of the permissions that the caller's role holds in it, such as `members.read`,
    /// in the order of the permission matrix. Each operation whose `x-access` is `workspace`
    /// names the permission it takes as its `x-permission`, and the roles' rank limits who may
    /// change or remove whom (SEC-09).
    pub(crate) permissions: Vec<String>,
    /// When it was created.
    pub(crate) created_at: Timestamp,
    // part: files
    /// The path of the public download of its picture, or null when it has none
    /// (WORKSPACES-022).
    pub(crate) picture_url: Option<String>,
    // end part: files
}

/// A workspace as the result of a keyed creation stored it, which a replay reads back, with the
/// permissions that the creation answered (PLATFORM-036). A result that holds none reads back
/// with those that its stored role holds, as a read of the workspace gives them: a deploy's two
/// releases read each other's results, and one whose workspaces carry no permissions stores
/// such a result (DATA-10).
#[derive(serde::Deserialize)]
struct Stored {
    id: WorkspaceId,
    name: WorkspaceName,
    role: Role,
    permissions: Option<Vec<String>>,
    created_at: Timestamp,
    // part: files
    picture_url: Option<String>,
    // end part: files
}

impl From<Stored> for Workspace {
    fn from(stored: Stored) -> Self {
        Self {
            id: stored.id,
            name: stored.name,
            role: stored.role,
            permissions: stored.permissions.unwrap_or_else(|| held(stored.role)),
            created_at: stored.created_at,
            // part: files
            picture_url: stored.picture_url,
            // end part: files
        }
    }
}

/// The names of the permissions that `role` holds, which a workspace shows the member whose role
/// it is: `access` gives them, as it decides what a role may do (WORKSPACES-007, SEC-08).
pub(crate) fn held(role: Role) -> Vec<String> {
    role.permissions().into_iter().map(str::to_owned).collect()
}

/// A workspace as it now stands, with the version its `ETag` names (PLATFORM-071). A creation of
/// one that a request with an idempotency key asks for stores it as the key's result
/// (PLATFORM-036).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub(crate) struct Current {
    /// The workspace.
    pub(crate) workspace: Workspace,
    /// Its row's version.
    pub(crate) version: Version,
}

/// What renaming a workspace takes (WORKSPACES-008).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rename {
    /// The new name.
    pub(crate) name: WorkspaceName,
}

/// A workspace the caller belongs to (WORKSPACES-003).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct WorkspaceItem {
    /// The workspace's ID.
    pub(crate) id: WorkspaceId,
    /// Its name.
    pub(crate) name: WorkspaceName,
    /// The caller's role in it.
    pub(crate) role: Role,
    /// The names of the permissions that the caller's role holds in it, as a read of the
    /// workspace gives them (WORKSPACES-007).
    pub(crate) permissions: Vec<String>,
    /// When the caller joined it.
    pub(crate) joined_at: Timestamp,
    // part: files
    /// The path of the public download of its picture, or null when it has none
    /// (WORKSPACES-022).
    pub(crate) picture_url: Option<String>,
    // end part: files
}

// part: superadmins
/// A workspace of the deployment, as a superadmin sees it: nothing of its content
/// (WORKSPACES-029).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct WorkspaceOverview {
    /// The workspace's ID.
    pub(crate) id: WorkspaceId,
    /// Its name.
    pub(crate) name: WorkspaceName,
    /// When it was created.
    pub(crate) created_at: Timestamp,
    /// How many members it has, its owner among them.
    pub(crate) member_count: i64,
    /// Its owner.
    pub(crate) owner: WorkspaceOwner,
}

/// A workspace's owner, as a superadmin sees them (WORKSPACES-029).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct WorkspaceOwner {
    /// The owner's user ID.
    pub(crate) user_id: UserId,
    /// Their name.
    pub(crate) name: DisplayName,
    /// Their email address.
    pub(crate) email: EmailAddress,
}
// end part: superadmins

/// A member of a workspace (WORKSPACES-004).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct Member {
    /// The member's user ID.
    pub(crate) user_id: UserId,
    /// Their name.
    pub(crate) name: DisplayName,
    /// Their email address.
    pub(crate) email: EmailAddress,
    /// Their role.
    pub(crate) role: Role,
    /// When they joined.
    pub(crate) joined_at: Timestamp,
    // part: files
    /// The path of the public download of their avatar, or null when they have none
    /// (WORKSPACES-010).
    pub(crate) avatar_url: Option<String>,
    // end part: files
}

/// A member as they now stand, with the version of their membership, which their `ETag` names
/// (WORKSPACES-011, PLATFORM-071).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrentMember {
    /// The member.
    pub(crate) member: Member,
    /// Their membership's version.
    pub(crate) version: Version,
}

/// What changing a member's role takes (WORKSPACES-012).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NewRole {
    /// The role the member is to hold.
    pub(crate) role: Role,
}

/// What transferring a workspace takes (WORKSPACES-015).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Transfer {
    /// The member who is to own the workspace.
    pub(crate) to: UserId,
}

/// A member's user ID, as a request's body names one: a UUID in its form of 36 characters, with
/// hyphens, whose letters RFC 9562 reads in either case (API-11, WORKSPACES-016).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemberId(UserId);

impl MemberId {
    /// The ID in `value`, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        read_id(value)
            .map(|id| Self(UserId::from_uuid(id)))
            .ok_or_else(|| Invalid::new("invalid", "A user ID is a UUID."))
    }

    /// The user the ID names.
    pub(crate) fn user_id(self) -> UserId {
        self.0
    }
}

impl PartialSchema for MemberId {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Uuid)))
            .min_length(Some(ID_CHARS))
            .max_length(Some(ID_CHARS))
            .into()
    }
}

impl ToSchema for MemberId {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("MemberId")
    }
}

/// A workspace whose only member is the user, as their export holds it (the accounts spec's
/// "What an export holds").
#[derive(Debug, Serialize)]
pub(super) struct ExportedWorkspace {
    pub(super) id: WorkspaceId,
    pub(super) name: WorkspaceName,
    pub(super) created_at: Timestamp,
    pub(super) updated_at: Timestamp,
}

// part: workspace_setup
/// The setup of a workspace whose only member is the user, as their export holds it: the name
/// its run was given (the accounts spec's "What an export holds").
#[derive(Debug, Serialize)]
pub(super) struct ExportedSetup {
    pub(super) workspace_id: WorkspaceId,
    pub(super) name: WorkspaceName,
    pub(super) created_at: Timestamp,
}
// end part: workspace_setup

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspaces_002_a_name_is_trimmed_collapsed_and_normalized() {
        let name = WorkspaceName::parse("  Acme \t\n  Labs  ").unwrap();
        assert_eq!(name.as_str(), "Acme Labs");
        // "e" followed by a combining acute accent becomes one character in form C.
        let composed = WorkspaceName::parse("Caf\u{65}\u{301}").unwrap();
        assert_eq!(composed.as_str(), "Caf\u{e9}");
    }

    #[test]
    fn workspaces_016_a_member_s_id_is_a_uuid_in_its_form_of_36_characters() {
        let id = "018f4e2a-7b1c-7def-8a3b-5c6d7e8f9a0b";
        assert_eq!(MemberId::parse(id).unwrap().user_id().to_string(), id);
        let capitals = MemberId::parse(&id.to_uppercase()).unwrap();
        assert_eq!(capitals.user_id().to_string(), id);
        for refused in ["", "nobody", "018f4e2a7b1c7def8a3b5c6d7e8f9a0b"] {
            assert_eq!(MemberId::parse(refused).unwrap_err().code, "invalid");
        }
    }

    #[test]
    fn data_10_a_stored_workspace_keeps_its_permissions_and_one_without_any_reads_its_role_s() {
        // A creation's result, as `commit_with` stores it, that holds no permissions, as a
        // release without the field stores it.
        let mut stored = serde_json::json!({
            "workspace": {
                "id": "01900000-0000-7000-8000-000000000000",
                "name": "Acme",
                "role": "admin",
                "created_at": "2026-11-01T10:00:00.000000Z",
            },
            "version": 1,
        });

        let current: Current = serde_json::from_value(stored.clone()).unwrap();

        assert_eq!(current.workspace.role, Role::Admin);
        assert_eq!(current.workspace.permissions, held(Role::Admin));
        assert!(
            current
                .workspace
                .permissions
                .contains(&"members.read".to_owned())
        );
        // A result that holds its permissions reads back with those, the first request's answer,
        // whatever its role holds in the matrix of the release that reads it (PLATFORM-036).
        stored["workspace"]["permissions"] = serde_json::json!(["workspace.read"]);
        let replayed: Current = serde_json::from_value(stored).unwrap();
        assert_eq!(replayed.workspace.permissions, ["workspace.read"]);
        assert_ne!(replayed.workspace.permissions, held(Role::Admin));
    }

    #[test]
    fn workspaces_002_a_name_holds_no_control_character() {
        for control in ["Ac\u{0}me", "Ac\u{1b}me", "Ac\u{7f}me", "Ac\u{9b}me"] {
            assert_eq!(
                WorkspaceName::parse(control).unwrap_err().code,
                "control_character"
            );
        }
        // A tab and a next line (U+0085) are whitespace, which is collapsed.
        let collapsed = WorkspaceName::parse("Acme\t\u{85}Labs").unwrap();
        assert_eq!(collapsed.as_str(), "Acme Labs");
    }

    #[test]
    fn workspaces_002_a_name_has_1_to_255_characters() {
        assert_eq!(WorkspaceName::parse(" \t ").unwrap_err().code, "required");
        assert!(WorkspaceName::parse(&"é".repeat(255)).is_ok());
        assert_eq!(
            WorkspaceName::parse(&"a".repeat(256)).unwrap_err().code,
            "too_long"
        );
    }
}
