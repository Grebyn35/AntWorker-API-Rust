//! Idempotency keys (API-12): the key a request sends, as draft-ietf-httpapi-idempotency-key-header-08
//! defines it, and the fingerprint of its request, which a unit of work claims as its first
//! statement and completes at its commit ([the platform spec](../../../docs/spec/platform.md#idempotency)).

use sha2::{Digest, Sha256};

use crate::telemetry::{metrics::Counter, names};

/// The most characters a key holds (PLATFORM-058).
const KEY_MAX_CHARS: usize = 255;

/// A request's idempotency key, with the fingerprint of the request it came with.
#[derive(Clone, PartialEq, Eq)]
pub struct IdempotencyKey {
    key: String,
    fingerprint: [u8; 32],
}

impl std::fmt::Debug for IdempotencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdempotencyKey").finish_non_exhaustive()
    }
}

impl IdempotencyKey {
    /// The key in the header's `value`, a String of structured fields (RFC 9651) of 1 to 255
    /// characters, for the request whose method, path, query and body digest are given, or
    /// `None` when the value isn't one (PLATFORM-058).
    pub(crate) fn parse(
        value: &[u8],
        method: &str,
        path: &str,
        query: &str,
        body_digest: &[u8; 32],
    ) -> Option<Self> {
        let key = structured_string(value)?;
        let length = key.chars().count();
        if length == 0 || length > KEY_MAX_CHARS {
            return None;
        }
        let mut hasher = Sha256::new();
        for part in [method.as_bytes(), path.as_bytes(), query.as_bytes()] {
            // Each part's length comes first, so no two requests' parts run together the same.
            hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(part);
        }
        hasher.update(body_digest);
        Some(Self {
            key,
            fingerprint: hasher.finalize().into(),
        })
    }

    /// The key.
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// The fingerprint of the request it came with.
    pub(crate) fn fingerprint(&self) -> &[u8; 32] {
        &self.fingerprint
    }
}

/// The String that `value` holds as a structured field: `"` and the characters from space to
/// `~`, with `"` and `\` escaped by `\`, then `"`, with spaces around it allowed (RFC 9651,
/// 3.3.3 and 4.2). Parameters after it aren't.
fn structured_string(value: &[u8]) -> Option<String> {
    let value = trim_spaces(value);
    let inner = value.strip_prefix(b"\"")?.strip_suffix(b"\"")?;
    let mut string = String::with_capacity(inner.len());
    let mut bytes = inner.iter();
    while let Some(&byte) = bytes.next() {
        match byte {
            b'\\' => match bytes.next() {
                Some(&escaped @ (b'"' | b'\\')) => string.push(char::from(escaped)),
                _ => return None,
            },
            b'"' => return None,
            0x20..=0x7e => string.push(char::from(byte)),
            _ => return None,
        }
    }
    Some(string)
}

fn trim_spaces(mut value: &[u8]) -> &[u8] {
    while let [b' ', rest @ ..] = value {
        value = rest;
    }
    while let [rest @ .., b' '] = value {
        value = rest;
    }
    value
}

/// What claiming a key found: no committed operation, so this one runs, or the result of the one
/// that committed under it (PLATFORM-018).
#[derive(Debug)]
pub enum Claimed<T> {
    /// No operation committed under the key, or the request has none: the operation runs.
    Fresh,
    /// The operation committed under the key with this result, which the request answers again.
    Replay(T),
}

/// How a claim ended, as `platform.idempotent_requests` counts it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Outcome {
    New,
    Replayed,
    Reused,
    InFlight,
}

static IDEMPOTENT_REQUESTS: Counter<1> = Counter::new(
    "platform.idempotent_requests",
    "{request}",
    "Requests with an idempotency key, by how claiming it ended: new, replayed, reused or \
     in_flight (PLATFORM-018).",
    [names::IDEMPOTENCY_OUTCOME],
);

/// Counts a claim that ended as `outcome`.
pub(crate) fn count(outcome: Outcome) {
    let label = match outcome {
        Outcome::New => "new",
        Outcome::Replayed => "replayed",
        Outcome::Reused => "reused",
        Outcome::InFlight => "in_flight",
    };
    IDEMPOTENT_REQUESTS.add(1, [label.into()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(value: &str) -> Option<String> {
        IdempotencyKey::parse(value.as_bytes(), "POST", "/v1/workspaces", "", &[0; 32])
            .map(|key| key.key)
    }

    #[test]
    fn platform_058_a_key_is_one_string_of_structured_fields() {
        assert_eq!(
            key(r#""8e03978e-40d5-43e8-bc93-6894a57f9324""#).as_deref(),
            Some("8e03978e-40d5-43e8-bc93-6894a57f9324")
        );
        assert_eq!(key(r#"  "a b"  "#).as_deref(), Some("a b"));
        assert_eq!(
            key(r#""say \"hi\" \\ bye""#).as_deref(),
            Some(r#"say "hi" \ bye"#)
        );
        for refused in [
            "8e03978e",
            r#""unterminated"#,
            r#""two" "strings""#,
            r#""bad \x escape""#,
            r#""key";a=1"#,
            "\"tab\there\"",
            "\"\"",
        ] {
            assert_eq!(key(refused), None, "{refused}");
        }
    }

    #[test]
    fn platform_058_a_key_has_1_to_255_characters() {
        assert!(key(&format!("\"{}\"", "k".repeat(255))).is_some());
        assert!(key(&format!("\"{}\"", "k".repeat(256))).is_none());
    }

    #[test]
    fn platform_018_the_fingerprint_covers_the_method_the_path_the_query_and_the_body() {
        let fingerprint = |method: &str, path: &str, query: &str, body: u8| {
            IdempotencyKey::parse(br#""k""#, method, path, query, &[body; 32])
                .unwrap()
                .fingerprint
        };
        let first = fingerprint("POST", "/v1/a", "b=1", 0);
        assert_eq!(first, fingerprint("POST", "/v1/a", "b=1", 0));
        assert_ne!(first, fingerprint("PUT", "/v1/a", "b=1", 0));
        assert_ne!(first, fingerprint("POST", "/v1/ab", "=1", 0));
        assert_ne!(first, fingerprint("POST", "/v1/a", "b=2", 0));
        assert_ne!(first, fingerprint("POST", "/v1/a", "b=1", 1));
    }
}
