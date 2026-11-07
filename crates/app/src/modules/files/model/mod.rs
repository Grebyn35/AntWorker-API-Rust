//! The file's domain types (D25), and what its routes answer.

mod kinds;

use std::borrow::Cow;

use platform::types::{Invalid, Timestamp, UserId, WorkspaceId, one_line};
use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

/// The most characters a file's name has (FILES-002).
const NAME_MAX_CHARS: usize = 255;

pub(crate) use kinds::Kind;

/// A file's name: one line of text of 1 to 255 characters, normalized as `one_line` says, with no
/// control character (FILES-002). A name is personal data, so `Debug` doesn't show it (OBS-04).
/// `Deserialize` only reads back a stored result that an idempotency key replays (PLATFORM-036),
/// which held a parsed name.
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
        let name = one_line(
            value,
            NAME_MAX_CHARS,
            "Name the file.",
            "A file's name has at most 255 characters.",
        )?;
        if name.chars().any(char::is_control) {
            return Err(Invalid::new(
                "control_character",
                "A file's name holds no control character.",
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

/// The settings of `files` (OPS-01).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Settings {
    /// The most bytes an upload holds: `FILE_MAX_BYTES` (FILES-004).
    pub(crate) max_bytes: usize,
    /// The days an object that no row names waits before its bytes go:
    /// `FILE_DELETION_DELAY_DAYS` (FILES-011).
    pub(crate) deletion_delay_days: u32,
}

/// A file of a workspace, as its routes answer it (FILES-001, FILES-008).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub(crate) struct File {
    /// The file's ID.
    pub(crate) id: uuid::Uuid,
    /// The workspace it belongs to.
    pub(crate) workspace_id: WorkspaceId,
    /// Its name.
    pub(crate) name: FileName,
    /// The media type of its kind (FILES-003).
    pub(crate) media_type: String,
    /// Its size, in bytes.
    pub(crate) size_bytes: u64,
    /// The member who uploaded it, until their account's deletion clears it.
    pub(crate) uploader_id: Option<UserId>,
    /// When it was uploaded.
    pub(crate) created_at: Timestamp,
}

/// A file as the routes that serve its bytes need it: the file, and the key of its object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stored {
    /// The file.
    pub(crate) file: File,
    /// The key of its object in the bucket.
    pub(crate) object_key: String,
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
    fn files_002_a_name_holds_no_control_character() {
        assert_eq!(
            FileName::parse("a\u{7}b.txt").unwrap_err().code,
            "control_character"
        );
        assert_eq!(
            FileName::parse("a\u{202e}b.txt").unwrap().as_str(),
            "a\u{202e}b.txt"
        );
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
