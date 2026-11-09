//! The user record's domain types (D25).

use std::borrow::Cow;

use platform::{
    errors::Internal,
    types::{EmailAddress, Invalid, Timestamp, UserId, Version, one_line},
};
use serde::Serialize;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

/// The most characters a display name has (ACCOUNTS-003).
const NAME_MAX_CHARS: usize = 100;

/// How a user appears to others: trimmed, with each run of whitespace inside collapsed to one
/// space, in Unicode Normalization Form C, of 1 to 100 characters (ACCOUNTS-003). It is personal
/// data, so `Debug` doesn't show it (OBS-04).
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct DisplayName(String);

impl std::fmt::Debug for DisplayName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DisplayName([redacted])")
    }
}

impl DisplayName {
    /// The name in `value`, normalized, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        one_line(
            value,
            NAME_MAX_CHARS,
            "Enter a name.",
            "A name has at most 100 characters.",
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

impl PartialSchema for DisplayName {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(1))
            .max_length(Some(NAME_MAX_CHARS))
            .into()
    }
}

impl ToSchema for DisplayName {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("DisplayName")
    }
}

/// The caller's own user record (ACCOUNTS-001).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct Profile {
    /// The user's ID.
    pub(crate) id: UserId,
    /// The user's email address.
    pub(crate) email: EmailAddress,
    /// The user's name.
    pub(crate) name: DisplayName,
    /// Whether the user proved they receive email at the address.
    pub(crate) email_verified: bool,
    // part: files
    /// The path of the public download of the user's avatar, or null when they have none
    /// (ACCOUNTS-011).
    pub(crate) avatar_url: Option<String>,
    // end part: files
}

/// The caller's own record, with its version, which its `ETag` names (ACCOUNTS-001, API-13).
#[derive(Clone, Debug)]
pub(crate) struct Current {
    /// The record.
    pub(crate) profile: Profile,
    /// Its version.
    pub(crate) version: Version,
}

/// What renaming oneself sets (ACCOUNTS-007).
#[derive(Clone, Debug)]
pub(crate) struct Rename {
    /// The new name.
    pub(crate) name: DisplayName,
}

/// How a user appears to the other members of a workspace they share (ACCOUNTS-005).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublicProfile {
    /// The user's ID.
    pub(crate) id: UserId,
    /// The user's email address.
    pub(crate) email: EmailAddress,
    /// The user's name.
    pub(crate) name: DisplayName,
    // part: files
    /// The path of the public download of the user's avatar, if they have one (ACCOUNTS-011).
    pub(crate) avatar_url: Option<String>,
    // end part: files
}

/// Where an export stands (ACCOUNTS-022).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExportState {
    /// Its archive is being built.
    Pending,
    /// Its archive is built, and downloads until it expires.
    Ready,
    /// The last attempt of its job failed, so it has no archive.
    Failed,
}

impl ExportState {
    /// The state named `name`, as the database's `data_export_state` names it.
    pub(super) fn from_name(name: &str) -> Result<Self, Internal> {
        match name {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            _ => Err(Internal::new(
                "an export's state that accounts doesn't know",
            )),
        }
    }
}

/// An export of the caller's data (ACCOUNTS-022).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = DataExport)]
pub(crate) struct Export {
    /// The export's ID.
    pub(crate) id: uuid::Uuid,
    /// Where it stands.
    pub(crate) state: ExportState,
    /// When it was requested.
    pub(crate) created_at: Timestamp,
    /// When its archive was built, or null until it is.
    pub(crate) ready_at: Option<Timestamp>,
    /// When it expires: 7 days after it became ready, or after its request while it isn't
    /// (D127).
    pub(crate) expires_at: Timestamp,
    /// The size of its archive, in bytes, or null until it is built.
    pub(crate) size_bytes: Option<u64>,
}

/// The user's record, as their export holds it (the accounts spec's "What an export holds").
#[derive(Clone, Debug, Serialize)]
pub(super) struct ExportedProfile {
    pub(super) id: UserId,
    pub(super) email: EmailAddress,
    pub(super) name: DisplayName,
    pub(super) email_verified: bool,
    pub(super) created_at: Timestamp,
    pub(super) updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_003_a_display_name_is_trimmed_collapsed_and_normalized() {
        let name = DisplayName::parse("  Ada \t\n  Lovelace  ").unwrap();
        assert_eq!(name.as_str(), "Ada Lovelace");
        // "e" followed by a combining acute accent becomes one character in form C.
        let composed = DisplayName::parse("Rene\u{301}e").unwrap();
        assert_eq!(composed.as_str(), "Ren\u{e9}e");
    }

    #[test]
    fn accounts_003_a_display_name_has_1_to_100_characters() {
        assert_eq!(DisplayName::parse(" \t ").unwrap_err().code, "required");
        assert!(DisplayName::parse(&"é".repeat(100)).is_ok());
        assert_eq!(
            DisplayName::parse(&"a".repeat(101)).unwrap_err().code,
            "too_long"
        );
    }
}
