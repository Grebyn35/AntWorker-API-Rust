//! The hashes that stand in for secrets in the database: SHA-256 of a token, which is too long to
//! guess, and HMAC-SHA-256 under a secret key of a value that is short or guessable, such as an
//! email or an IP address (SEC-03, D32).

use std::fmt;
#[cfg(feature = "test-support")]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use hmac::{
    Hmac, KeyInit, Mac,
    digest::{CtOutput, Output},
};
use sha2::{Digest, Sha256};

/// The fewest bytes `HMAC_KEY` holds: SHA-256's output, which HMAC's key should match.
pub const MIN_KEY_BYTES: usize = 32;

/// The SHA-256 hash of `token`, which the database stores in its place (SEC-03).
pub fn sha256(token: &[u8]) -> [u8; 32] {
    Sha256::digest(token).into()
}

/// The key of every keyed hash, from `HMAC_KEY` (SEC-03). Its `Debug` reveals nothing.
#[derive(Clone)]
pub struct HmacKey {
    mac: Hmac<Sha256>,
    /// How many keyed hashes the key and its clones have made, which the tests read.
    #[cfg(feature = "test-support")]
    hashes: Arc<AtomicUsize>,
}

/// A keyed hash that [`HmacKey::tag`] made, to compare with stored ones.
pub struct Tag(CtOutput<Hmac<Sha256>>);

impl Tag {
    /// Whether `stored` is this keyed hash, compared in constant time, so a wrong one's answer
    /// tells nothing of the right one.
    pub fn is(&self, stored: &[u8]) -> bool {
        <[u8; 32]>::try_from(stored)
            .is_ok_and(|stored| self.0 == CtOutput::new(Output::<Hmac<Sha256>>::from(stored)))
    }
}

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
        let mac = Hmac::new_from_slice(bytes).map_err(|_| ShortKey)?;
        Ok(Self {
            mac,
            #[cfg(feature = "test-support")]
            hashes: Arc::default(),
        })
    }

    /// The keyed hash of `value` as a value of `kind`, such as the IP bucket's. The kind keeps
    /// equal values of different kinds apart, such as a user ID and an email that read alike.
    pub fn hash(&self, kind: &str, value: &[u8]) -> [u8; 32] {
        self.mac(kind, value).finalize().into_bytes().into()
    }

    /// Whether `tag` is the keyed hash of `value` as a value of `kind`, compared in constant
    /// time, so a wrong tag's answer tells nothing of the right one.
    pub fn verify(&self, kind: &str, value: &[u8], tag: &[u8]) -> bool {
        self.mac(kind, value).verify_slice(tag).is_ok()
    }

    /// The keyed hash of `value` as a value of `kind`, to compare with each of several stored
    /// ones: one keyed hash, however many it is compared with.
    pub fn tag(&self, kind: &str, value: &[u8]) -> Tag {
        Tag(self.mac(kind, value).finalize())
    }

    /// How many keyed hashes this key and its clones have made, which a test reads to compare
    /// the hashing work of two requests (SEC-05).
    #[cfg(feature = "test-support")]
    pub fn hashes(&self) -> usize {
        self.hashes.load(Ordering::SeqCst)
    }

    /// The MAC of `value` as a value of `kind`, before it is finalized, counted as one keyed hash.
    fn mac(&self, kind: &str, value: &[u8]) -> Hmac<Sha256> {
        #[cfg(feature = "test-support")]
        self.hashes.fetch_add(1, Ordering::SeqCst);
        let mut mac = self.mac.clone();
        mac.update(kind.as_bytes());
        mac.update(&[0]);
        mac.update(value);
        mac
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
    fn sec_03_a_tag_is_the_keyed_hash_and_no_other_value() {
        let key = HmacKey::new(&[7; 32]).unwrap();
        let tag = key.tag("code", b"123456");
        assert!(tag.is(&key.hash("code", b"123456")));
        assert!(!tag.is(&key.hash("code", b"123457")));
        assert!(!tag.is(&key.hash("code", b"123456")[..31]));
    }

    #[test]
    fn ops_01_an_hmac_key_has_at_least_32_bytes() {
        assert!(HmacKey::new(&[1; 31]).is_err());
        assert!(HmacKey::new(&[1; 32]).is_ok());
    }

    #[test]
    fn sec_01_an_hmac_key_never_prints_its_bytes() {
        assert!(!format!("{:?}", HmacKey::new(&[1; 32]).unwrap()).contains('1'));
    }
}
