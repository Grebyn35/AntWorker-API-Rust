//! The workspace's domain types (D25), and what its routes answer.

use std::borrow::Cow;

use platform::types::{EmailAddress, Invalid, Timestamp, UserId, WorkspaceId};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
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
/// result that an idempotency key replays (PLATFORM-018), which held a parsed name.
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
        let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let name: String = collapsed.nfc().collect();
        if name.is_empty() {
            return Err(Invalid::new("required", "Enter a name."));
        }
        if name.chars().count() > NAME_MAX_CHARS {
            return Err(Invalid::new(
                "too_long",
                "A workspace name has at most 255 characters.",
            ));
        }
        Ok(Self(name))
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

/// A workspace just created, with its creator's role (WORKSPACES-001).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub(crate) struct Created {
    /// The workspace's ID.
    pub(crate) id: WorkspaceId,
    /// Its name.
    pub(crate) name: WorkspaceName,
    /// The creator's role: `owner`.
    pub(crate) role: Role,
    /// When it was created.
    pub(crate) created_at: Timestamp,
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
