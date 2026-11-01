//! The hashes that stand in for secrets in the database: SHA-256 of a token, which is too long to
//! guess, and HMAC-SHA-256 under a secret key of a value that is short or guessable, such as an
//! email or an IP address (SEC-03, D32).

use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

/// The fewest bytes `HMAC_KEY` holds: SHA-256's output, which HMAC's key should match.
pub const MIN_KEY_BYTES: usize = 32;

/// The SHA-256 hash of `token`, which the database stores in its place (SEC-03).
pub fn sha256(token: &[u8]) -> [u8; 32] {
    Sha256::digest(token).into()
}

/// The key of every keyed hash, from `HMAC_KEY` (SEC-03). Its `Debug` reveals nothing.
#[derive(Clone)]
pub struct HmacKey(Hmac<Sha256>);

/// A key shorter than [`MIN_KEY_BYTES`].
#[derive(Debug, thiserror::Error)]
#[error("an HMAC key has at least 32 bytes")]
pub struct ShortKey;

impl HmacKey {
    /// The key of `bytes`, which hold at least [`MIN_KEY_BYTES`].
    pub fn new(bytes: &[u8]) -> Result<Self, ShortKey> {
        if bytes.len() < MIN_KEY_BYTES {
            return Err(ShortKey);
        }
        // HMAC takes a key of any length, so this never fails.
        Hmac::new_from_slice(bytes).map(Self).map_err(|_| ShortKey)
    }

    /// The keyed hash of `value` as a value of `kind`, such as the IP bucket's. The kind keeps
    /// equal values of different kinds apart, such as a user ID and an email that read alike.
    pub fn hash(&self, kind: &str, value: &[u8]) -> [u8; 32] {
        let mut mac = self.0.clone();
        mac.update(kind.as_bytes());
        mac.update(&[0]);
        mac.update(value);
        mac.finalize().into_bytes().into()
    }
}

impl fmt::Debug for HmacKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HmacKey([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec_03_a_keyed_hash_depends_on_the_key_the_kind_and_the_value() {
        let key = HmacKey::new(&[7; 32]).unwrap();
        let other = HmacKey::new(&[8; 32]).unwrap();
        let hash = key.hash("ip", b"192.0.2.1");
        assert_eq!(hash, key.hash("ip", b"192.0.2.1"));
        assert_ne!(hash, other.hash("ip", b"192.0.2.1"));
        assert_ne!(hash, key.hash("user", b"192.0.2.1"));
        assert_ne!(hash, key.hash("ip", b"192.0.2.2"));
    }

    #[test]
    fn sec_03_a_key_has_at_least_32_bytes() {
        assert!(HmacKey::new(&[1; 31]).is_err());
        assert!(!format!("{:?}", HmacKey::new(&[1; 32]).unwrap()).contains('1'));
    }
}
