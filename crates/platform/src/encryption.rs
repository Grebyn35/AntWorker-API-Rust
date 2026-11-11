//! The encryption of the secrets that the database holds and the API reads back, such as an
//! authenticator app's: AES-256-GCM under a ring of versioned keys, each ciphertext bound to the
//! row that owns it (SEC-02, PLATFORM-086, PLATFORM-087).

use std::{collections::BTreeMap, fmt};

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{errors::Internal, random, types::Secret};

/// How many bytes a key holds: AES-256's.
pub const KEY_BYTES: usize = 32;

/// How many bytes a nonce holds: the 96 bits that GCM takes.
const NONCE_BYTES: usize = 12;

/// The keys of `ENCRYPTION_KEYS`, by version, and the version that encrypts (PLATFORM-086). Its
/// `Debug` reveals no key (SEC-01).
#[derive(Clone)]
pub struct Keyring {
    keys: BTreeMap<i16, Aes256Gcm>,
    current: i16,
}

/// Why a key ring can't be built from its settings.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyringError {
    /// `ENCRYPTION_KEYS` doesn't list keys as `<version>:<base64 of 32 bytes>`, separated by
    /// commas, with no version twice.
    #[error("the key ring's keys aren't versions and keys")]
    Keys,
    /// `ENCRYPTION_KEY_VERSION` names no version of the ring.
    #[error("the version that encrypts isn't one of the ring's")]
    Current,
}

impl Keyring {
    /// The ring of `keys`, such as `1:<base64>,2:<base64>`, in which `current` encrypts, or the
    /// highest version when it is `None` (PLATFORM-086).
    pub fn parse(keys: &str, current: Option<i16>) -> Result<Self, KeyringError> {
        let mut ring = BTreeMap::new();
        for entry in keys.split(',') {
            let (version, key) = entry.trim().split_once(':').ok_or(KeyringError::Keys)?;
            let version: i16 = version.trim().parse().map_err(|_| KeyringError::Keys)?;
            if version < 1 {
                return Err(KeyringError::Keys);
            }
            let bytes = STANDARD
                .decode(key.trim())
                .map_err(|_| KeyringError::Keys)?;
            if bytes.len() != KEY_BYTES {
                return Err(KeyringError::Keys);
            }
            let cipher = Aes256Gcm::new_from_slice(&bytes).map_err(|_| KeyringError::Keys)?;
            if ring.insert(version, cipher).is_some() {
                return Err(KeyringError::Keys);
            }
        }
        let highest = *ring.keys().next_back().ok_or(KeyringError::Keys)?;
        let current = current.unwrap_or(highest);
        if !ring.contains_key(&current) {
            return Err(KeyringError::Current);
        }
        Ok(Self {
            keys: ring,
            current,
        })
    }

    /// The version that encrypts.
    pub fn current(&self) -> i16 {
        self.current
    }

    /// Whether the ring holds `version`, which a stored ciphertext names.
    pub fn holds(&self, version: i16) -> bool {
        self.keys.contains_key(&version)
    }

    /// `secret` encrypted under the current version, with a new random nonce, bound to `binding`
    /// (PLATFORM-087).
    pub fn seal(&self, binding: &Binding, secret: &[u8]) -> Result<Sealed, Internal> {
        let cipher = self
            .keys
            .get(&self.current)
            .ok_or_else(|| Internal::new("the key ring lacks its current version"))?;
        let nonce = random::bytes::<NONCE_BYTES>()?;
        let associated = binding.associated_data();
        let ciphertext = cipher
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: secret,
                    aad: &associated,
                },
            )
            .map_err(|_| Internal::new("a secret couldn't be encrypted"))?;
        let mut bytes = Vec::with_capacity(NONCE_BYTES + ciphertext.len());
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&ciphertext);
        Ok(Sealed {
            version: self.current,
            bytes,
        })
    }

    /// The secret that `sealed` holds, decrypted under its own version, when it is bound to
    /// `binding` and unchanged; an error for another binding, a version the ring lacks, or
    /// changed bytes (PLATFORM-087).
    pub fn open(&self, binding: &Binding, sealed: &Sealed) -> Result<Secret<Vec<u8>>, Internal> {
        let cipher = self.keys.get(&sealed.version).ok_or_else(|| {
            Internal::new(format!(
                "a secret is encrypted under version {} of the key ring, which ENCRYPTION_KEYS \
                 lacks",
                sealed.version
            ))
        })?;
        let (nonce, ciphertext) = sealed
            .bytes
            .split_at_checked(NONCE_BYTES)
            .ok_or_else(|| Internal::new("a stored secret is shorter than its nonce"))?;
        let nonce: [u8; NONCE_BYTES] = nonce
            .try_into()
            .map_err(|_| Internal::new("a stored secret's nonce has the wrong length"))?;
        let associated = binding.associated_data();
        cipher
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: ciphertext,
                    aad: &associated,
                },
            )
            .map(Secret::new)
            .map_err(|_| Internal::new("a stored secret doesn't open under its key and its row"))
    }
}

impl fmt::Debug for Keyring {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keyring")
            .field("versions", &self.keys.keys().collect::<Vec<_>>())
            .field("current", &self.current)
            .finish_non_exhaustive()
    }
}

/// What a ciphertext is bound to: the table, the column and the ID of the row that owns its
/// secret, which exists before the secret is written (SEC-02).
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    table: &'static str,
    column: &'static str,
    row: uuid::Uuid,
}

impl Binding {
    /// The binding of the secret in `column` of the row `row` of `table`.
    pub fn new(table: &'static str, column: &'static str, row: uuid::Uuid) -> Self {
        Self { table, column, row }
    }

    /// The associated data of the ciphertext: the table, the column and the row, each apart from
    /// the next by a zero byte, which no name holds.
    fn associated_data(&self) -> Vec<u8> {
        [
            self.table.as_bytes(),
            &[0],
            self.column.as_bytes(),
            &[0],
            self.row.as_bytes(),
        ]
        .concat()
    }
}

/// A secret as a row stores it: the version of the key that encrypted it, and its nonce and its
/// ciphertext, with the tag that GCM appends.
#[derive(Clone, PartialEq, Eq)]
pub struct Sealed {
    /// The key's version.
    pub version: i16,
    /// The nonce, then the ciphertext and its tag.
    pub bytes: Vec<u8>,
}

impl fmt::Debug for Sealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sealed")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> String {
        STANDARD.encode([byte; KEY_BYTES])
    }

    fn row() -> uuid::Uuid {
        uuid::Uuid::from_u128(7)
    }

    #[test]
    fn platform_086_a_ring_holds_versions_and_keys_and_the_highest_encrypts_by_default() {
        let ring = Keyring::parse(&format!("1:{}, 3:{}", key(1), key(3)), None).unwrap();
        assert_eq!(ring.current(), 3);
        assert!(ring.holds(1) && ring.holds(3) && !ring.holds(2));
        let ring = Keyring::parse(&format!("1:{},3:{}", key(1), key(3)), Some(1)).unwrap();
        assert_eq!(ring.current(), 1);
        assert_eq!(
            Keyring::parse(&format!("1:{}", key(1)), Some(2)).unwrap_err(),
            KeyringError::Current
        );
    }

    #[test]
    fn platform_086_a_ring_refuses_what_isn_t_versions_and_keys_of_32_bytes() {
        let short = STANDARD.encode([1; 31]);
        for refused in [
            String::new(),
            key(1),
            format!("0:{}", key(1)),
            format!("-1:{}", key(1)),
            format!("32768:{}", key(1)),
            format!("1:{short}"),
            format!("1:{},1:{}", key(1), key(2)),
            format!("1:{},", key(1)),
            "1:not base64".to_owned(),
        ] {
            assert_eq!(
                Keyring::parse(&refused, None).unwrap_err(),
                KeyringError::Keys,
                "{refused}"
            );
        }
    }

    #[test]
    fn platform_087_a_sealed_secret_opens_under_its_version_and_its_binding_alone() {
        let ring = Keyring::parse(&format!("1:{},2:{}", key(1), key(2)), Some(1)).unwrap();
        let binding = Binding::new("totp_factors", "secret", row());
        let sealed = ring.seal(&binding, b"the secret").unwrap();
        assert_eq!(sealed.version, 1);
        assert_eq!(
            ring.open(&binding, &sealed).unwrap().expose(),
            b"the secret"
        );
        // A ring whose current version is another still opens it under its own.
        let next = Keyring::parse(&format!("1:{},2:{}", key(1), key(2)), None).unwrap();
        assert_eq!(
            next.open(&binding, &sealed).unwrap().expose(),
            b"the secret"
        );
        // Another row, column or table opens nothing.
        for other in [
            Binding::new("totp_factors", "secret", uuid::Uuid::from_u128(8)),
            Binding::new("totp_factors", "other", row()),
            Binding::new("totp_setups", "secret", row()),
        ] {
            assert!(ring.open(&other, &sealed).is_err());
        }
        // A ring without its version opens nothing, nor do changed bytes.
        let without = Keyring::parse(&format!("2:{}", key(2)), None).unwrap();
        assert!(without.open(&binding, &sealed).is_err());
        let mut changed = sealed.clone();
        if let Some(last) = changed.bytes.last_mut() {
            *last ^= 1;
        }
        assert!(ring.open(&binding, &changed).is_err());
        assert!(
            ring.open(
                &binding,
                &Sealed {
                    version: 1,
                    bytes: vec![0; 4]
                }
            )
            .is_err()
        );
    }

    #[test]
    fn platform_087_two_seals_of_one_secret_differ() {
        let ring = Keyring::parse(&format!("1:{}", key(1)), None).unwrap();
        let binding = Binding::new("totp_factors", "secret", row());
        let first = ring.seal(&binding, b"the secret").unwrap();
        let second = ring.seal(&binding, b"the secret").unwrap();
        assert_ne!(first.bytes, second.bytes);
        assert_ne!(&first.bytes[NONCE_BYTES..], b"the secret");
    }

    #[test]
    fn sec_01_a_ring_and_a_sealed_secret_print_no_key_or_byte() {
        let ring = Keyring::parse(&format!("1:{}", key(9)), None).unwrap();
        let printed = format!("{ring:?}");
        assert!(
            !printed.contains(&key(9)) && !printed.contains("9, 9"),
            "{printed}"
        );
        let sealed = ring
            .seal(&Binding::new("t", "c", row()), b"the secret")
            .unwrap();
        assert!(!format!("{sealed:?}").contains("bytes"));
    }
}
