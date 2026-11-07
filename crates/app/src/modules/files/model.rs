//! The file's domain types (D25), and what its routes answer.

use std::borrow::Cow;

use platform::types::{Invalid, Timestamp, Version, one_line};
use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

/// The most characters a file's name has (FILES-002).
const NAME_MAX_CHARS: usize = 255;

/// A file's name: one line of text of 1 to 255 characters, normalized as `one_line` says
/// (FILES-002). A name is personal data, so `Debug` doesn't show it (OBS-04). `Deserialize`
/// only reads back a stored result that an idempotency key replays (PLATFORM-036), which held a
/// parsed name.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct FileName(String);

impl std::fmt::Debug for FileName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileName([redacted])")
    }
}

impl FileName {
    /// The name in `value`, normalized, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        one_line(
            value,
            NAME_MAX_CHARS,
            "Enter a name.",
            "A file's name has at most 255 characters.",
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

impl PartialSchema for FileName {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(1))
            .max_length(Some(NAME_MAX_CHARS))
            .into()
    }
}

impl ToSchema for FileName {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("FileName")
    }
}

/// A file that a member creates (FILES-001).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NewFile {
    /// Its name.
    pub(crate) name: FileName,
}

/// What renaming a file takes (FILES-005).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rename {
    /// The new name.
    pub(crate) name: FileName,
}

/// A file, as its routes answer it (FILES-001, FILES-003).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub(crate) struct File {
    /// The file's ID.
    pub(crate) id: uuid::Uuid,
    /// Its name.
    pub(crate) name: FileName,
    /// When it was created.
    pub(crate) created_at: Timestamp,
    /// When it last changed.
    pub(crate) updated_at: Timestamp,
}

/// A file as it now stands, with the version its `ETag` names (PLATFORM-071), which a create
/// with an idempotency key stores as its result (PLATFORM-036).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Current {
    /// The file.
    pub(crate) file: File,
    /// Its row's version.
    pub(crate) version: Version,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_002_a_name_is_trimmed_collapsed_and_normalized() {
        let name = FileName::parse("  Blue \t\n  one  ").unwrap();
        assert_eq!(name.as_str(), "Blue one");
        // "e" followed by a combining acute accent becomes one character in form C.
        let composed = FileName::parse("Caf\u{65}\u{301}").unwrap();
        assert_eq!(composed.as_str(), "Caf\u{e9}");
    }

    #[test]
    fn files_002_a_name_has_1_to_255_characters() {
        assert_eq!(FileName::parse(" \t ").unwrap_err().code, "required");
        assert!(FileName::parse(&"é".repeat(255)).is_ok());
        assert_eq!(
            FileName::parse(&"a".repeat(256)).unwrap_err().code,
            "too_long"
        );
    }
}
