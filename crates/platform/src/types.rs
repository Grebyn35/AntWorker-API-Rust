//! Types shared by every module: secrets, the IDs of the tenancy keys and of sessions, and the
//! email address (D25).

use std::{borrow::Cow, fmt};

use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{KnownFormat, ObjectBuilder, RefOr, Schema, SchemaFormat, Type},
};
use uuid::Uuid;

/// A value that is never printed (SEC-01). `Debug` shows a placeholder, and there is no
/// `Display` or serialization.
#[derive(Clone)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps `value`.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The wrapped value, for the one place that uses it. Never log or display it.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// Who sent a request: its client IP (PLATFORM-047) and its user agent, which audit entries
/// record (AUDIT-003).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    /// The client's address.
    pub ip: std::net::IpAddr,
    /// The `User-Agent` header, when it is text.
    pub user_agent: Option<String>,
}

/// Why a domain type refused a value: a stable `snake_case` code, which clients act on, and a
/// sentence for humans (API-10, ERR-09).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invalid {
    /// The stable reason, such as `too_long`.
    pub code: &'static str,
    /// The reason, for humans.
    pub message: Cow<'static, str>,
}

impl Invalid {
    /// A refusal for the reason `code`, explained by `message`.
    pub fn new(code: &'static str, message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// The ID the database generated (DATA-08).
            pub fn from_uuid(id: Uuid) -> Self {
                Self(id)
            }

            /// The ID as a UUID, for a query's parameter.
            pub fn uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl PartialSchema for $name {
            fn schema() -> RefOr<Schema> {
                uuid_schema()
            }
        }

        impl ToSchema for $name {
            fn name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($name))
            }
        }
    };
}

id_type!(
    /// A user's ID, one of the two tenancy keys `platform` knows (architecture §4).
    UserId
);
id_type!(
    /// A workspace's ID, the other tenancy key.
    WorkspaceId
);
id_type!(
    /// A session's ID.
    SessionId
);

fn uuid_schema() -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::Uuid)))
        .into()
}

/// A moment, which the database's clock gave (DATA-03), written as RFC 3339 in UTC with a `Z`,
/// at microsecond precision (API-11).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(time::OffsetDateTime);

impl Timestamp {
    /// The moment `at`, as the database returned it.
    pub fn from_database(at: time::OffsetDateTime) -> Self {
        Self(at)
    }

    /// The moment, for arithmetic such as how long until it comes.
    pub fn at(self) -> time::OffsetDateTime {
        self.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = self.0.to_offset(time::UtcOffset::UTC);
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
            at.year(),
            u8::from(at.month()),
            at.day(),
            at.hour(),
            at.minute(),
            at.second(),
            at.microsecond()
        )
    }
}

impl Serialize for Timestamp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl PartialSchema for Timestamp {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
            .into()
    }
}

impl ToSchema for Timestamp {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Timestamp")
    }
}

/// The most characters an email address has (ACCOUNTS-002).
const EMAIL_MAX_CHARS: usize = 254;

/// An email address, trimmed and lowercased, of 1 to 254 characters, that is valid as the HTML
/// standard defines one for `<input type="email">` (ACCOUNTS-002).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct EmailAddress(String);

impl EmailAddress {
    /// The address in `value`, normalized, or why it isn't one.
    pub fn parse(value: &str) -> Result<Self, Invalid> {
        let address = value.trim().to_lowercase();
        if address.is_empty() {
            return Err(Invalid::new("required", "Enter an email address."));
        }
        if address.chars().count() > EMAIL_MAX_CHARS {
            return Err(Invalid::new(
                "too_long",
                "An email address has at most 254 characters.",
            ));
        }
        if !is_valid_email(&address) {
            return Err(Invalid::new(
                "invalid_email",
                "This isn't an email address.",
            ));
        }
        Ok(Self(address))
    }

    /// The normalized address.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// An address the database holds, which was normalized when it was stored.
    pub fn from_stored(address: String) -> Self {
        Self(address)
    }
}

impl PartialSchema for EmailAddress {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Email)))
            .min_length(Some(1))
            .max_length(Some(EMAIL_MAX_CHARS))
            .into()
    }
}

impl ToSchema for EmailAddress {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("EmailAddress")
    }
}

/// The HTML standard's valid email address: a local part of letters, digits and
/// ``.!#$%&'*+/=?^_`{|}~-``, then `@`, then labels separated by dots, each of 1 to 63 letters,
/// digits and hyphens that neither starts nor ends with a hyphen.
fn is_valid_email(address: &str) -> bool {
    let Some((local, domain)) = address.split_once('@') else {
        return false;
    };
    let local_ok = !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(c));
    local_ok && domain.split('.').all(is_valid_label)
}

fn is_valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec_01_a_secret_never_prints_its_value() {
        let secret = Secret::new("hunter2".to_owned());
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert!(!format!("{secret:#?}").contains("hunter2"));
        assert_eq!(secret.expose(), "hunter2");
    }

    #[test]
    fn api_11_a_timestamp_is_rfc_3339_in_utc_at_microseconds() {
        let at = time::OffsetDateTime::from_unix_timestamp_nanos(1_793_500_000_123_456_789)
            .unwrap()
            .to_offset(time::UtcOffset::from_hms(2, 0, 0).unwrap());
        assert_eq!(
            Timestamp::from_database(at).to_string(),
            "2026-11-01T02:26:40.123456Z"
        );
    }

    #[test]
    fn accounts_002_an_email_address_is_trimmed_and_lowercased() {
        let address = EmailAddress::parse("  Alice.Smith+tag@Example.COM \n").unwrap();
        assert_eq!(address.as_str(), "alice.smith+tag@example.com");
    }

    #[test]
    fn accounts_002_an_email_address_has_1_to_254_characters() {
        assert_eq!(EmailAddress::parse("   ").unwrap_err().code, "required");
        let longest = format!("{}@example.com", "a".repeat(254 - "@example.com".len()));
        assert!(EmailAddress::parse(&longest).is_ok());
        let too_long = format!("a{longest}");
        assert_eq!(EmailAddress::parse(&too_long).unwrap_err().code, "too_long");
    }

    #[test]
    fn accounts_002_an_email_address_is_valid_as_html_defines_it() {
        for valid in [
            "a@b",
            "x!#$%&'*+/=?^_`{|}~-.y@sub-1.example.org",
            "a@localhost",
        ] {
            assert!(EmailAddress::parse(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "plain",
            "@example.com",
            "a@",
            "a@@b.com",
            "a b@c.com",
            "a@-b.com",
            "a@b-.com",
            "a@b..com",
            "\"quoted\"@example.com",
            "ä@example.com",
        ] {
            assert_eq!(
                EmailAddress::parse(invalid).unwrap_err().code,
                "invalid_email",
                "{invalid}"
            );
        }
        let long_label = format!("a@{}.com", "b".repeat(64));
        assert_eq!(
            EmailAddress::parse(&long_label).unwrap_err().code,
            "invalid_email"
        );
    }
}
