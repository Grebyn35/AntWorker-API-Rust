//! The key of an object in the bucket, and whether a request's URL names its object
//! (PLATFORM-094).

use super::StorageError;
use crate::{errors::Internal, random};

/// The key of an object: random, so it names no row and reveals nothing, under a prefix of its
/// module's (the files spec's "Storage").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// A new key under `prefix`: 128 bits from the operating system's secure random source, in
    /// hexadecimal (CODE-06).
    pub fn random(prefix: &str) -> Result<Self, Internal> {
        let bits = random::bytes::<16>()?;
        let hex: String = bits.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self(format!("{prefix}{hex}")))
    }

    /// A key that the database holds.
    pub fn from_stored(key: &str) -> Self {
        Self(key.to_owned())
    }

    /// The key.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The key, when a request's URL names its object: each of its segments, between slashes,
    /// holds a character, and none is `.` or `..`. A URL's path drops or resolves any other, so
    /// its request would name another object, or the bucket itself, and such a key is refused
    /// before a URL is built.
    pub(super) fn addressed(&self) -> Result<&str, StorageError> {
        if self.is_addressed() {
            Ok(&self.0)
        } else {
            Err(StorageError::Failed(Internal::new(
                "the key has a segment that a URL's path doesn't keep",
            )))
        }
    }

    /// Whether a request's URL names the key's object, which no key that the API makes fails;
    /// a listing of versions gives a key that fails it as the bucket lists it (PLATFORM-102).
    pub fn is_addressed(&self) -> bool {
        self.0
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | ".."))
    }
}

#[cfg(test)]
mod tests {
    use rusty_s3::{Bucket, UrlStyle};

    use super::*;

    fn bucket() -> Bucket {
        let endpoint = "https://s3.example.com".parse().unwrap();
        Bucket::new(endpoint, UrlStyle::Path, "files", "eu-1").unwrap()
    }

    #[test]
    fn platform_094_a_key_is_addressed_when_a_url_s_path_keeps_each_of_its_segments() {
        let bucket = bucket();
        for key in [
            "avatars/0123456789abcdef0123456789abcdef",
            "a",
            "a/b/c",
            "...",
            ".a",
            "a.",
            "a b+c%d",
            "%2e%2e/a",
            "a?b#c",
            "a\\b",
            "a/\u{e9}",
        ] {
            let key = ObjectKey::from_stored(key);
            let object = key.addressed().unwrap();
            // The URL's path holds the key, encoded, and nothing a parser resolved away.
            let path = bucket.object_url(object).path().to_owned();
            let decoded = percent_encoding::percent_decode_str(&path)
                .decode_utf8()
                .unwrap()
                .into_owned();
            assert_eq!(decoded, format!("/files/{}", key.as_str()), "{key:?}");
        }
        for key in [
            "", ".", "..", "a/../b", "a/./b", "/a", "a/", "a//b", "../files",
        ] {
            let key = ObjectKey::from_stored(key);
            assert!(
                matches!(key.addressed(), Err(StorageError::Failed(_))),
                "{key:?}"
            );
        }
    }
}
