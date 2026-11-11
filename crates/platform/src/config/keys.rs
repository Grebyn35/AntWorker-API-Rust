//! The ring of the keys that encrypt the secrets the database holds and the API reads back
//! (SEC-02, PLATFORM-086), which `serve` reads with the rest of its settings.

use std::{ffi::OsString, io, path::Path};

use super::{ConfigError, Problem, Settings};
use crate::encryption::{Keyring, KeyringError};

pub(super) const KEYS_EXPECTED: &str = "keys separated by commas, each a version from 1 to \
     32767, a colon and the base64 of 32 random bytes, such as 1:<key>, with no version twice";

pub(super) const VERSION_EXPECTED: &str = "one of the versions of ENCRYPTION_KEYS";

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// The ring of `ENCRYPTION_KEYS`, in which `ENCRYPTION_KEY_VERSION` encrypts, or the highest
    /// version when it is unset (PLATFORM-086).
    pub(super) fn keyring(&self) -> Result<Keyring, ConfigError> {
        let current = self.optional_with("ENCRYPTION_KEY_VERSION", VERSION_EXPECTED, |value| {
            value.parse::<i16>().ok()
        })?;
        let keys = self.with("ENCRYPTION_KEYS", KEYS_EXPECTED, |value| {
            Some(value.to_owned())
        })?;
        Keyring::parse(&keys, current).map_err(|error| match error {
            KeyringError::Keys => {
                ConfigError::new("ENCRYPTION_KEYS", Problem::Invalid(KEYS_EXPECTED))
            }
            KeyringError::Current => {
                ConfigError::new("ENCRYPTION_KEY_VERSION", Problem::Invalid(VERSION_EXPECTED))
            }
        })
    }
}
