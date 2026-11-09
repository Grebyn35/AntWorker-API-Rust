//! The workspace's domain types (D25), and what its routes answer.

use std::borrow::Cow;

use platform::types::{EmailAddress, Invalid, Timestamp, UserId, Version, WorkspaceId, one_line};
use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

use crate::modules::{access::Role, accounts::DisplayName};

/// The most characters a workspace name has (WORKSPACES-002).
const NAME_MAX_CHARS: usize = 255;

/// A workspace's name: trimmed, with each run of whitespace inside collapsed to one space, in
/// Unicode Normalization Form C, of 1 to 255 characters (WORKSPACES-002). A name can hold a
/// person's name, so `Debug` doesn't show it (OBS-04). `Deserialize` only reads back a stored
/// result that an idempotency key replays (PLATFORM-036), which held a parsed name.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// A workspace that a user creates (WORKSPACES-001).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewWorkspace {
    /// Its name.
    pub(crate) name: WorkspaceName,
}

/// A workspace, as one of its members reads it, with their role: its creator's, `owner`, when
/// creating it answers it (WORKSPACES-001, WORKSPACES-007).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub(crate) struct Workspace {
    /// The workspace's ID.
    pub(crate) id: WorkspaceId,
    /// Its name.
    pub(crate) name: WorkspaceName,
    /// The caller's role in it.
    pub(crate) role: Role,
    /// When it was created.
    pub(crate) created_at: Timestamp,
}

/// A workspace as it now stands, with the version its `ETag` names (PLATFORM-071), which a
/// create with an idempotency key stores as its result (PLATFORM-036).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// When the caller joined it.
    pub(crate) joined_at: Timestamp,
}

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
    fn workspaces_002_a_name_has_1_to_255_characters() {
        assert_eq!(WorkspaceName::parse(" \t ").unwrap_err().code, "required");
        assert!(WorkspaceName::parse(&"é".repeat(255)).is_ok());
        assert_eq!(
            WorkspaceName::parse(&"a".repeat(256)).unwrap_err().code,
            "too_long"
        );
    }
}
