//! Types shared by every module: secrets, the IDs of the tenancy keys and of sessions, a row's
//! version, and the email address (D25).

use std::{borrow::Cow, fmt};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
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

/// Who sent a request: its client IP (PLATFORM-053) and its user agent, which audit entries
/// record (AUDIT-003). Both are personal data, so `Debug` shows neither (OBS-04).
#[derive(Clone, PartialEq, Eq)]
pub struct Client {
    /// The client's address.
    pub ip: std::net::IpAddr,
    /// The `User-Agent` header, when it is text.
    pub user_agent: Option<String>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Client([redacted])")
    }
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

/// `value`, of at most `max_chars` characters as given, as one line of text, such as a name or a
/// search: trimmed, with each run of whitespace inside it collapsed to one space, in Unicode
/// Normalization Form C (NFC), of 1 to `max_chars` characters, and with no control character
/// left, such as U+0000, which Postgres stores in no text; or why it isn't one: `required`,
/// explained by `empty`, when the line holds no character, `too_long`, explained by `too_long`,
/// when `value` or the line holds more than `max_chars`, or `control_character` (D25, API-10). A
/// control character that is whitespace, such as a tab, is collapsed with the whitespace around
/// it. A domain type of text a user writes wraps what it returns.
pub fn one_line(
    value: &str,
    max_chars: usize,
    empty: &'static str,
    too_long: &'static str,
) -> Result<String, Invalid> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let line: String = collapsed.nfc().collect();
    if line.is_empty() {
        return Err(Invalid::new("required", empty));
    }
    if value.chars().count() > max_chars || line.chars().count() > max_chars {
        return Err(Invalid::new("too_long", too_long));
    }
    if line.chars().any(char::is_control) {
        return Err(Invalid::new(
            "control_character",
            "This holds a control character.",
        ));
    }
    Ok(line)
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
    /// A user's ID, a tenancy key that `platform` knows (architecture §4).
    UserId
);
id_type!(
    /// A session's ID.
    SessionId
);
id_type!(
    /// A workspace's ID, the other tenancy key that `platform` knows (architecture §4).
    WorkspaceId
);

/// The schema of a UUID that may name a row of any kind, such as the resource of an audit
/// entry, for a parameter's documentation (API-11).
pub struct AnyId;

impl PartialSchema for AnyId {
    fn schema() -> RefOr<Schema> {
        uuid_schema()
    }
}

impl ToSchema for AnyId {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("AnyId")
    }
}

/// How many characters a UUID holds in the form that the API reads.
pub const ID_CHARS: usize = 36;

/// The UUID that `value` holds in the one form that the API reads: 36 characters, with hyphens,
/// whose letters RFC 9562 reads in either case, the form that a schema's `format: uuid` documents
/// (API-11). The forms that the `uuid` crate also parses, without hyphens, in braces and as a
/// URN, hold none, so a row has one ID that names it.
pub fn read_id(value: &str) -> Option<Uuid> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one reader of a UUID that a request gives (API-11)"
    )]
    let id = Uuid::try_parse(value).ok();
    id.filter(|_| value.len() == ID_CHARS)
}

fn uuid_schema() -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::Uuid)))
        .into()
}

/// A moment, written as RFC 3339 in UTC with a `Z`, at microsecond precision (API-11).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(time::OffsetDateTime);

impl Timestamp {
    /// The moment `at`, which a row holds, or which the code computes from a row's, such as the
    /// start of an authenticator app's last used time step (DATA-03).
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

/// Reads back what `Serialize` writes, such as a stored result that an idempotency key replays
/// (PLATFORM-036), and nothing else.
impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| serde::de::Error::custom("not a timestamp the API wrote"))
    }
}

impl Timestamp {
    /// The moment in `text`, as a request gives one, such as a list's filter, or `invalid`:
    /// `YYYY-MM-DDTHH:MM:SS`, with an uppercase `T`, then, or not, a `.` and 1 to 6 digits of a
    /// second's fraction, then an uppercase `Z`, which names a day of the calendar and a second
    /// of 00 to 59 (API-11, PLATFORM-088).
    pub fn read(text: &str) -> Result<Self, Invalid> {
        Self::parse_with(text, 0).ok_or_else(|| {
            Invalid::new(
                "invalid",
                "A moment is written YYYY-MM-DDTHH:MM:SSZ, in UTC, with up to 6 digits of a \
                 second's fraction before the Z, such as 2026-11-16T09:30:00.250Z.",
            )
        })
    }

    /// The moment in `text`, as `Display` writes it: `YYYY-MM-DDTHH:MM:SS.ffffffZ`.
    fn parse(text: &str) -> Option<Self> {
        Self::parse_with(text, 6)
    }

    /// The moment in `text`, `YYYY-MM-DDTHH:MM:SS`, then a fraction of a second of at least
    /// `digits` and at most 6 digits, after a `.` unless it has none, then `Z`.
    fn parse_with(text: &str, digits: usize) -> Option<Self> {
        let (seconds, fraction) = text.strip_suffix('Z')?.split_at_checked(19)?;
        let bytes = seconds.as_bytes();
        let separators = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')];
        let shape_ok = bytes.iter().enumerate().all(|(index, &byte)| {
            separators
                .iter()
                .find(|&&(at, _)| at == index)
                .map_or(byte.is_ascii_digit(), |&(_, separator)| byte == separator)
        });
        let fraction = match fraction.strip_prefix('.') {
            None if fraction.is_empty() => "",
            Some(fraction) if !fraction.is_empty() => fraction,
            _ => return None,
        };
        if !shape_ok
            || !(digits..=6).contains(&fraction.len())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let number = |range: std::ops::Range<usize>| seconds.get(range)?.parse::<u32>().ok();
        let micros = format!("{fraction:0<6}").parse::<u32>().ok()?;
        let month = time::Month::try_from(u8::try_from(number(5..7)?).ok()?).ok()?;
        let date = time::Date::from_calendar_date(
            i32::try_from(number(0..4)?).ok()?,
            month,
            u8::try_from(number(8..10)?).ok()?,
        )
        .ok()?;
        let time = time::Time::from_hms_micro(
            u8::try_from(number(11..13)?).ok()?,
            u8::try_from(number(14..16)?).ok()?,
            u8::try_from(number(17..19)?).ok()?,
            micros,
        )
        .ok()?;
        Some(Self(date.with_time(time).assume_utc()))
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

/// A version of an editable resource, which its `ETag` names (PLATFORM-071), such as the
/// `version` of its row, which the shared trigger raises on every update (DATA-02), or one that
/// a request's `If-Match` names, which may be no row's. It serializes as its number, as a stored
/// result holds it (PLATFORM-036).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Version(i64);

impl Version {
    // part: client_telemetry
    /// Version 0: the version of a resource that has no row until its first change, which its
    /// module's spec gives it until that change commits, as a user's consent to the telemetry of
    /// their clients has (the platform spec's Preconditions).
    pub const UNWRITTEN: Self = Self(0);
    // end part: client_telemetry

    /// The version `value`, as the database returned it.
    pub fn from_database(value: i64) -> Self {
        Self(value)
    }

    /// The version, for a statement that keeps an update to it (DATA-18).
    pub fn value(self) -> i64 {
        self.0
    }

    /// The value of the `ETag` header that names this version: the number, quoted, a strong
    /// entity tag (API-13).
    pub fn etag(self) -> String {
        format!("\"{}\"", self.0)
    }

    /// The version whose [`Version::etag`] is `tag`, character for character, as RFC 9110's
    /// strong comparison matches tags, or `None`: `"03"` names none, since version 3's tag is
    /// `"3"` (PLATFORM-071).
    pub fn from_etag(tag: &str) -> Option<Self> {
        let digits = tag.strip_prefix('"')?.strip_suffix('"')?;
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let version = Self(digits.parse().ok()?);
        (version.etag() == tag).then_some(version)
    }
}

/// The most characters an email address has (ACCOUNTS-002).
const EMAIL_MAX_CHARS: usize = 254;

/// An email address, of at most 254 characters as given, trimmed and lowercased, of 1 to 254
/// characters, that is valid as the HTML standard defines one for `<input type="email">`
/// (ACCOUNTS-002). It is personal data, so
/// `Debug` doesn't show it (OBS-04). `Deserialize` reads it back through [`EmailAddress::parse`],
/// such as from a job's payload.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct EmailAddress(String);

impl<'de> Deserialize<'de> for EmailAddress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(|_| serde::de::Error::custom("not an email address"))
    }
}

impl fmt::Debug for EmailAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EmailAddress([redacted])")
    }
}

impl EmailAddress {
    /// The address in `value`, normalized, or why it isn't one.
    pub fn parse(value: &str) -> Result<Self, Invalid> {
        let address = value.trim().to_lowercase();
        if address.is_empty() {
            return Err(Invalid::new("required", "Enter an email address."));
        }
        if value.chars().count() > EMAIL_MAX_CHARS || address.chars().count() > EMAIL_MAX_CHARS {
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
    fn api_11_an_id_is_read_in_its_form_of_36_characters_alone() {
        let id = "019a0000-0000-7000-8000-00000000abcd";
        let read = read_id(id).unwrap();

        assert_eq!(read.to_string(), id);
        assert_eq!(read_id(&id.to_uppercase()), Some(read));
        for other in [
            "019a000000007000800000000000abcd",
            "{019a0000-0000-7000-8000-00000000abcd}",
            "urn:uuid:019a0000-0000-7000-8000-00000000abcd",
            " 019a0000-0000-7000-8000-00000000abcd",
            "019a0000-0000-7000-8000-00000000abc",
            "019a0000-0000-7000-8000-00000000abcg",
            "",
        ] {
            assert_eq!(read_id(other), None, "{other}");
        }
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
    fn platform_036_a_timestamp_reads_back_what_it_writes() {
        let at =
            time::OffsetDateTime::from_unix_timestamp_nanos(1_793_500_000_123_456_000).unwrap();
        let written = serde_json::to_value(Timestamp::from_database(at)).unwrap();
        let read: Timestamp = serde_json::from_value(written).unwrap();
        assert_eq!(read, Timestamp::from_database(at));
        assert!(serde_json::from_str::<Timestamp>("\"2026-11-01T02:26:40Z\"").is_err());
    }

    #[test]
    fn platform_088_a_filter_s_moment_is_rfc_3339_in_utc_to_the_microsecond_at_most() {
        let at = |nanos: i128| {
            Ok(Timestamp::from_database(
                time::OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap(),
            ))
        };
        assert_eq!(
            Timestamp::read("2026-11-01T02:26:40Z"),
            at(1_793_500_000_000_000_000)
        );
        assert_eq!(
            Timestamp::read("2026-11-01T02:26:40.25Z"),
            at(1_793_500_000_250_000_000)
        );
        assert_eq!(
            Timestamp::read("2026-11-01T02:26:40.123456Z"),
            at(1_793_500_000_123_456_000)
        );
        for refused in [
            "2026-11-01T02:26:40.1234567Z",
            "2026-11-01T02:26:40.Z",
            "2026-11-01T02:26:40",
            "2026-11-01T02:26:40+00:00",
            "2026-11-01 02:26:40Z",
            "2026-+1-01T02:26:40Z",
            "2026-02-30T02:26:40Z",
            "2026-11-01T24:00:00Z",
            "2026-11-01T02:26:40.12a4Z",
            "2026-11-01T02:26:4é0Z",
            "2026-11-01t02:26:40Z",
            "2026-11-01T02:26:40z",
            "2016-12-31T23:59:60Z",
            "",
        ] {
            assert_eq!(
                Timestamp::read(refused).unwrap_err().code,
                "invalid",
                "{refused}"
            );
        }
    }

    #[test]
    fn obs_04_an_email_address_and_a_client_never_print_through_debug() {
        let address = EmailAddress::parse("ada@example.com").unwrap();
        assert!(!format!("{address:?}").contains("ada"));
        let client = Client {
            ip: "203.0.113.9".parse().unwrap(),
            user_agent: Some("Desktop/1.2".to_owned()),
        };
        let shown = format!("{client:?}");
        assert!(!shown.contains("203.0.113.9") && !shown.contains("Desktop"));
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
    fn accounts_002_an_email_address_holds_at_most_254_characters_as_given() {
        // Of the bound's length, then once more with white space before or after it, which was
        // taken once trimmed.
        let longest = format!("{}@example.com", "a".repeat(254 - "@example.com".len()));
        assert!(EmailAddress::parse(&longest).is_ok());
        for padded in [format!(" {longest}"), format!("{longest}\n")] {
            assert_eq!(EmailAddress::parse(&padded).unwrap_err().code, "too_long");
        }
        // Within the bound, with white space around it, which trimming removes.
        let padded = format!(" {}@Example.com ", "a".repeat(240));
        assert_eq!(padded.chars().count(), 254);
        let expected = format!("{}@example.com", "a".repeat(240));
        assert_eq!(EmailAddress::parse(&padded).unwrap().as_str(), expected);
    }

    #[test]
    fn api_10_one_line_holds_at_most_its_bound_as_given_and_once_normalized() {
        assert_eq!(one_line(" a  b ", 6, "", "").unwrap(), "a b");
        // Trailing white space, inner white space, and an e and a combining accent, which NFC
        // makes one character: each normalizes to fewer than 6 characters, from 7 as given.
        for over in ["abcdef ", "a     b", "e\u{301}e\u{301}e\u{301}e"] {
            assert_eq!(
                one_line(over, 6, "", "").unwrap_err().code,
                "too_long",
                "{over:?}"
            );
        }
        // U+0958, which NFC writes as two characters, U+0915 U+093C: 3 of them make a line of 6
        // characters once normalized, and 4 one of 8, each within the 6 as given.
        assert_eq!(
            one_line(&"\u{958}".repeat(3), 6, "", "").unwrap(),
            "\u{915}\u{93c}".repeat(3)
        );
        assert_eq!(
            one_line(&"\u{958}".repeat(4), 6, "", "").unwrap_err().code,
            "too_long"
        );
        // White space alone is no line, whatever its length.
        assert_eq!(
            one_line(&" ".repeat(9), 6, "", "").unwrap_err().code,
            "required"
        );
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
