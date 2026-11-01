//! The domain types of sign-in (D25): passwords, device IDs and how a client takes its tokens.

use std::{borrow::Cow, collections::HashSet, sync::LazyLock, time::Duration};

use platform::types::{Invalid, Secret, SessionId, Timestamp};
use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

/// The fewest characters a password has (AUTH-003).
const PASSWORD_MIN_CHARS: usize = 8;

/// The most characters a password has (AUTH-003).
const PASSWORD_MAX_CHARS: usize = 128;

/// The shortest run of ascending or descending digits a password may not contain (AUTH-003).
const DIGIT_RUN: usize = 8;

/// The passwords that breaches revealed most often, lowercased, of 8 to 128 characters: the
/// 100,000 that Pwned Passwords counted most, as the UK's National Cyber Security Centre published
/// them, from SecLists (MIT, `breached-passwords.LICENSE` beside the list).
const BREACHED_PASSWORDS: &str = include_str!("../../../data/breached-passwords.txt");

/// The words that a password of this product may not contain, lowercased, such as its name
/// (ASVS 6.2.11). `just rename` rewrites the product's name here too (ARCH-15).
const CONTEXT_WORDS: &[&str] = &["antworker"];

/// The breached-password list as a set, built the first time a password is checked: a read-only
/// value initialized once (ARCH-12).
static BREACHED: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| BREACHED_PASSWORDS.lines().collect());

/// A password that AUTH-003 allows, kept exactly as the user typed it (ASVS 6.2.8).
#[derive(Clone, Debug)]
pub(crate) struct Password(Secret<String>);

impl Password {
    /// The password `value`, or why AUTH-003 refuses it.
    pub(crate) fn parse(value: String) -> Result<Self, Invalid> {
        let length = value.chars().count();
        if length < PASSWORD_MIN_CHARS {
            return Err(Invalid::new(
                "too_short",
                "A password has at least 8 characters.",
            ));
        }
        if length > PASSWORD_MAX_CHARS {
            return Err(Invalid::new(
                "too_long",
                "A password has at most 128 characters.",
            ));
        }
        let lowered = value.to_lowercase();
        if BREACHED.contains(lowered.as_str()) {
            return Err(Invalid::new(
                "breached",
                "This password is among those that breaches revealed. Choose another.",
            ));
        }
        if CONTEXT_WORDS.iter().any(|word| lowered.contains(word)) {
            return Err(Invalid::new(
                "context_word",
                "A password may not contain the product's name.",
            ));
        }
        if is_one_repeated_character(&value) {
            return Err(Invalid::new(
                "repeated_character",
                "A password may not be one character repeated.",
            ));
        }
        if has_digit_run(&value) {
            return Err(Invalid::new(
                "digit_run",
                "A password may not contain 8 or more ascending or descending digits in a row.",
            ));
        }
        Ok(Self(Secret::new(value)))
    }

    /// A password as the user typed it at sign-in, of 1 to 128 characters, which is checked
    /// against its hash rather than the policy, so a password that was allowed when it was set
    /// still signs in.
    pub(crate) fn presented(value: String) -> Result<Self, Invalid> {
        let length = value.chars().count();
        if length == 0 {
            return Err(Invalid::new("required", "Enter your password."));
        }
        if length > PASSWORD_MAX_CHARS {
            return Err(Invalid::new(
                "too_long",
                "A password has at most 128 characters.",
            ));
        }
        Ok(Self(Secret::new(value)))
    }

    /// A password the API chose itself, such as the one behind the dummy hash (AUTH-005).
    pub(crate) fn generated(value: String) -> Self {
        Self(Secret::new(value))
    }

    /// The password's bytes, for hashing. Never log them.
    pub(crate) fn bytes(&self) -> &[u8] {
        self.0.expose().as_bytes()
    }
}

fn is_one_repeated_character(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| chars.all(|c| c.to_lowercase().eq(first.to_lowercase())))
}

fn has_digit_run(value: &str) -> bool {
    let digits: Vec<i32> = value
        .chars()
        .map(|c| {
            c.to_digit(10)
                .map_or(-10, |digit| i32::try_from(digit).unwrap_or(-10))
        })
        .collect();
    [1, -1].into_iter().any(|step| {
        let mut run = 1;
        digits.windows(2).any(|pair| {
            run = if pair[0] >= 0 && pair[1] == pair[0] + step {
                run + 1
            } else {
                1
            };
            run >= DIGIT_RUN
        })
    })
}

impl PartialSchema for Password {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
                utoipa::openapi::KnownFormat::Password,
            )))
            .min_length(Some(PASSWORD_MIN_CHARS))
            .max_length(Some(PASSWORD_MAX_CHARS))
            .into()
    }
}

impl ToSchema for Password {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Password")
    }
}

/// The most characters a device ID has.
const DEVICE_ID_MAX_CHARS: usize = 64;

/// An opaque ID a client chooses for its installation: 1 to 64 characters from `[A-Za-z0-9_-]`
/// (the auth spec's Device IDs).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct DeviceId(String);

impl DeviceId {
    /// The device ID `value`, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        let valid = (1..=DEVICE_ID_MAX_CHARS).contains(&value.len())
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(Invalid::new(
                "invalid_device_id",
                "A device ID has 1 to 64 letters, digits, underscores and hyphens.",
            ))
        }
    }

    /// A device ID the database holds.
    pub(super) fn from_stored(value: String) -> Self {
        Self(value)
    }

    /// The ID.
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialSchema for DeviceId {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(1))
            .max_length(Some(DEVICE_ID_MAX_CHARS))
            .pattern(Some("^[A-Za-z0-9_-]+$"))
            .into()
    }
}

impl ToSchema for DeviceId {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("DeviceId")
    }
}

/// How a client takes its session's tokens (AUTH-015).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TokenDelivery {
    /// In the response's body, for a client that sets its own headers.
    #[default]
    Body,
    /// As cookies that no script can read, for a browser.
    Cookie,
}

/// How long a session's tokens last (AUTH-007).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lifetimes {
    /// How long an access token lasts.
    pub(crate) access: Duration,
    /// How long a refresh token lasts, sliding with each refresh.
    pub(crate) refresh: Duration,
    /// How long a session lasts at most, whatever its refreshes.
    pub(crate) absolute: Duration,
}

/// A session's two new tokens, returned once (AUTH-007).
#[derive(Clone)]
pub(crate) struct Tokens {
    /// The access token.
    pub(crate) access: Secret<String>,
    /// The refresh token.
    pub(crate) refresh: Secret<String>,
}

/// A session, as its client sees it.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(crate) struct Session {
    /// The session's ID.
    pub(crate) id: SessionId,
    /// The device ID the client signed in with, if any.
    pub(crate) device_id: Option<DeviceId>,
    /// When the session began.
    pub(crate) created_at: Timestamp,
    /// When its access token expires.
    pub(crate) access_token_expires_at: Timestamp,
    /// When its refresh token expires, unless a refresh slides it.
    pub(crate) refresh_token_expires_at: Timestamp,
    /// When the session ends, whatever its refreshes.
    pub(crate) expires_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(password: &str) -> &'static str {
        Password::parse(password.to_owned()).unwrap_err().code
    }

    #[test]
    fn auth_003_a_password_has_8_to_128_characters_of_any_kind() {
        assert_eq!(refusal("Shrt7!x"), "too_short");
        assert!(Password::parse("é∂ƒ©˙∆˚¬".to_owned()).is_ok());
        assert!(Password::parse("x".repeat(127) + "y").is_ok());
        assert_eq!(refusal(&("x".repeat(128) + "y")), "too_long");
    }

    #[test]
    fn auth_003_a_breached_password_is_refused_whatever_its_case() {
        assert_eq!(refusal("password1"), "breached");
        assert_eq!(refusal("PassWord1"), "breached");
    }

    #[test]
    fn auth_003_a_password_with_a_context_word_is_refused() {
        assert_eq!(refusal("my-AntWorker-pass"), "context_word");
    }

    #[test]
    fn auth_003_one_repeated_character_is_refused() {
        assert_eq!(refusal("QqQqQqQqQqQqQq"), "repeated_character");
    }

    #[test]
    fn auth_003_a_run_of_8_ascending_or_descending_digits_is_refused() {
        assert_eq!(refusal("x23456789y"), "digit_run");
        assert_eq!(refusal("q98765432q"), "digit_run");
        assert!(Password::parse("x1234567y-sea".to_owned()).is_ok());
        assert!(Password::parse("13579246810-q".to_owned()).is_ok());
    }

    #[test]
    fn auth_003_a_password_is_kept_exactly_as_typed() {
        let password = Password::parse("  Spaced Out 42  ".to_owned());
        assert_eq!(password.unwrap().bytes(), b"  Spaced Out 42  ");
    }

    #[test]
    fn auth_003_the_breached_list_holds_at_least_3000_passwords_of_8_or_more_characters() {
        assert!(BREACHED.len() >= 3000);
        assert!(BREACHED.iter().all(|password| {
            (8..=128).contains(&password.chars().count()) && password.to_lowercase() == *password
        }));
    }
}
