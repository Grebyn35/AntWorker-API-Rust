//! The user record's domain types (D25).

use std::borrow::Cow;

use platform::types::{EmailAddress, Invalid, UserId};
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;
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
        let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let name: String = collapsed.nfc().collect();
        if name.is_empty() {
            return Err(Invalid::new("required", "Enter a name."));
        }
        if name.chars().count() > NAME_MAX_CHARS {
            return Err(Invalid::new(
                "too_long",
                "A name has at most 100 characters.",
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
