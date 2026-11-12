//! The domain types of sign-in (D25): passwords, device IDs and how a client takes its tokens;
//! in `codes.rs`, the verification, reset and change codes sent to an address, and recovery codes;
//! in `second_factors.rs`, an authenticator app's secret and codes; and in `sessions.rs`, a
//! device's name and a session as a user's list of them shows it.

use std::{borrow::Cow, collections::HashSet, sync::LazyLock, time::Duration};

use platform::{
    errors::Internal,
    types::{Invalid, Secret, SessionId, Timestamp, UserId},
};
use serde::Serialize;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

mod codes;
mod second_factors;
mod sessions;

pub(crate) use codes::{ChangeCode, RecoveryCode, ResetCode, VerificationCode};
pub(super) use codes::{Code, Purpose};
pub(crate) use second_factors::{AppCode, AppSecret, FactorProof, STEP_SECONDS};
pub(crate) use sessions::{DeviceName, SessionItem};

/// The fewest characters a password has (AUTH-003).
const PASSWORD_MIN_CHARS: usize = 8;

/// The most characters a password has (AUTH-003).
const PASSWORD_MAX_CHARS: usize = 128;

/// The shortest run of ascending or descending digits a password may not contain (AUTH-003).
const DIGIT_RUN: usize = 8;

/// The passwords that breaches revealed most often, lowercased, of 8 to 128 characters: the
/// 100,000 that Pwned Passwords counted most, as the UK's National Cyber Security Centre published
/// them, from SecLists (MIT, `breached-passwords.LICENSE` beside the list).
const BREACHED_PASSWORDS: &str = include_str!("../../../../data/breached-passwords.txt");

/// The words that a password of this product may not contain, lowercased, such as its name
/// (ASVS 6.2.11). `just rename` rewrites the product's name here too (ARCH-15).
const CONTEXT_WORDS: &[&str] = &["antworker"];

/// The breached-password list as a set: a read-only value initialized once (ARCH-12), which
/// startup builds through [`load_breached_passwords`].
static BREACHED: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| BREACHED_PASSWORDS.lines().collect());

/// Builds the breached-password set, CPU work that startup runs on a blocking thread, so no
/// request waits for it (CODE-05).
pub(crate) fn load_breached_passwords() {
    LazyLock::force(&BREACHED);
}

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

/// How a client takes its session's tokens: a browser, which every request with an `Origin`
/// comes from, as cookies, and any other client in the body (AUTH-015, SEC-07).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenDelivery {
    /// In the response's body, for a client that sets its own headers.
    Body,
    /// As cookies that no script can read, for a browser.
    Cookie,
}

/// How long a session's tokens last (AUTH-007).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Lifetimes {
    /// How long an access token lasts, at most as long as its refresh token (AUTH-007).
    pub(crate) access: Duration,
    /// How long a refresh token lasts, sliding with each refresh, at most as long as its session.
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

/// What the name of a browser's device token's cookie starts with, after `__Host-` (AUTH-015).
pub(crate) const DEVICE_COOKIE: &str = "device_token_";

/// A device token, returned once, with when it expires (AUTH-019).
#[derive(Clone)]
pub(crate) struct DeviceToken {
    /// Its ID, which its audit entry names.
    pub(crate) id: uuid::Uuid,
    /// The name of the cookie, after `__Host-`, that holds it in a browser: its email's (AUTH-015).
    pub(crate) cookie: String,
    /// The token.
    pub(crate) token: Secret<String>,
    /// When it expires.
    pub(crate) expires_at: Timestamp,
}

/// What a session's account must still do before the API serves it anything else: prove its email
/// address (AUTH-025, AUTH-026), or set up a second factor (AUTH-070).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Required {
    /// While email is on, the account hasn't proved its address.
    pub(crate) email_verification: bool,
    /// While the deployment requires a second factor, the account has none.
    pub(crate) second_factor_setup: bool,
}

/// A session as the database holds it, before the service adds what its account must still do
/// (AUTH-026, AUTH-070).
#[derive(Clone, Debug)]
pub(crate) struct StoredSession {
    pub(crate) id: SessionId,
    pub(crate) user_id: UserId,
    pub(crate) device_id: Option<DeviceId>,
    pub(crate) device_name: Option<DeviceName>,
    pub(crate) provider: Option<String>,
    pub(crate) created_at: Timestamp,
    pub(crate) access_token_expires_at: Timestamp,
    pub(crate) refresh_token_expires_at: Timestamp,
    pub(crate) expires_at: Timestamp,
    pub(crate) authenticated_at: Timestamp,
}

impl StoredSession {
    /// The session as its client sees it, whose account must still do what `required` says
    /// (AUTH-026, AUTH-070).
    pub(crate) fn answer(self, required: Required) -> Session {
        Session {
            id: self.id,
            user_id: self.user_id,
            device_id: self.device_id,
            device_name: self.device_name,
            provider: self.provider,
            created_at: self.created_at,
            access_token_expires_at: self.access_token_expires_at,
            refresh_token_expires_at: self.refresh_token_expires_at,
            expires_at: self.expires_at,
            authenticated_at: self.authenticated_at,
            email_verification_required: required.email_verification,
            second_factor_setup_required: required.second_factor_setup,
        }
    }
}

/// A session, as its client sees it.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(crate) struct Session {
    /// The session's ID.
    pub(crate) id: SessionId,
    /// The user the session signs in.
    pub(crate) user_id: UserId,
    /// The device ID the client signed in with, if any.
    pub(crate) device_id: Option<DeviceId>,
    /// The name the client gave its device, if any (AUTH-042).
    pub(crate) device_name: Option<DeviceName>,
    /// The provider whose sign-in started the session, by its key, or null for a password
    /// (AUTH-073).
    pub(crate) provider: Option<String>,
    /// When the session began.
    pub(crate) created_at: Timestamp,
    /// When its access token expires.
    pub(crate) access_token_expires_at: Timestamp,
    /// When its refresh token expires, unless a refresh slides it.
    pub(crate) refresh_token_expires_at: Timestamp,
    /// When the session ends, whatever its refreshes.
    pub(crate) expires_at: Timestamp,
    /// When its user last proved who they are in it, their password and the second factor of an
    /// account that has one: when it began, re-authenticated, or changed the password (AUTH-021,
    /// AUTH-040, AUTH-065).
    pub(crate) authenticated_at: Timestamp,
    /// Whether its account must prove its email address, with the code sent to it, before the
    /// API serves it anything but proving it: true while the deployment sends email and the
    /// address isn't proven (AUTH-025, AUTH-026).
    pub(crate) email_verification_required: bool,
    /// Whether its account must set up an authenticator app before the API serves it anything
    /// but that and proving its address: true while the deployment requires a second factor and
    /// the account has none (AUTH-070).
    pub(crate) second_factor_setup_required: bool,
}

/// A way to sign in, as the database's `identity_method` names it (DATA-04).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum IdentityMethod {
    /// An email and a password (AUTH-001).
    Password,
}

impl IdentityMethod {
    /// The method named `name`, which a query read.
    pub(super) fn from_name(name: &str) -> Result<Self, Internal> {
        match name {
            "password" => Ok(Self::Password),
            _ => Err(Internal::new("an identity's method that auth doesn't know")),
        }
    }
}

/// A way to sign in, as its user's export holds it, without a password's hash (the accounts spec's
/// "What an export holds").
#[derive(Debug, Serialize)]
pub(super) struct ExportedIdentity {
    pub(super) method: IdentityMethod,
    pub(super) created_at: Timestamp,
    pub(super) updated_at: Timestamp,
}

/// An account of the user's at an OAuth provider, as their export holds it (AUTH-073).
#[derive(Debug, Serialize)]
pub(super) struct ExportedProviderIdentity {
    pub(super) id: uuid::Uuid,
    pub(super) provider: String,
    pub(super) subject: String,
    pub(super) created_at: Timestamp,
}

/// A session, as its user's export holds it, without its tokens' hashes.
#[derive(Debug, Serialize)]
pub(super) struct ExportedSession {
    pub(super) id: uuid::Uuid,
    pub(super) device_id: Option<String>,
    pub(super) device_name: Option<String>,
    pub(super) provider: Option<String>,
    pub(super) created_at: Timestamp,
    pub(super) updated_at: Timestamp,
    pub(super) authenticated_at: Timestamp,
    pub(super) access_token_expires_at: Timestamp,
    pub(super) refresh_token_expires_at: Timestamp,
    pub(super) expires_at: Timestamp,
}

/// A verification code, as its user's export holds it, without its hash (SEC-03).
#[derive(Debug, Serialize)]
pub(super) struct ExportedCode {
    pub(super) id: uuid::Uuid,
    pub(super) purpose: Purpose,
    pub(super) created_at: Timestamp,
    pub(super) expires_at: Timestamp,
}

/// An authenticator app, as its user's export holds it, without its secret (SEC-02).
#[derive(Debug, Serialize)]
pub(super) struct ExportedApp {
    pub(super) id: uuid::Uuid,
    pub(super) created_at: Timestamp,
    pub(super) updated_at: Timestamp,
    pub(super) last_used_step: i64,
}

/// A setup of an authenticator app, as its user's export holds it, without its secret.
#[derive(Debug, Serialize)]
pub(super) struct ExportedSetup {
    pub(super) id: uuid::Uuid,
    pub(super) created_at: Timestamp,
    pub(super) expires_at: Timestamp,
}

/// A recovery code, as its user's export holds it, without its hash (SEC-03).
#[derive(Debug, Serialize)]
pub(super) struct ExportedRecoveryCode {
    pub(super) id: uuid::Uuid,
    pub(super) created_at: Timestamp,
}

/// A sign-in that waits for its second factor, as its user's export holds it, without its token's
/// hash or its device token's.
#[derive(Debug, Serialize)]
pub(super) struct ExportedChallenge {
    pub(super) id: uuid::Uuid,
    pub(super) device_id: Option<DeviceId>,
    pub(super) device_name: Option<DeviceName>,
    pub(super) provider: Option<String>,
    pub(super) created_at: Timestamp,
    pub(super) expires_at: Timestamp,
}

/// A device token, as its user's export holds it, without its hash or its email's.
#[derive(Debug, Serialize)]
pub(super) struct ExportedDeviceToken {
    pub(super) id: uuid::Uuid,
    pub(super) created_at: Timestamp,
    pub(super) expires_at: Timestamp,
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
