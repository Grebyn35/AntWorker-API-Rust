//! The settings of the outbound clients (SEC-10), and the settings that a module reads itself,
//! such as an OAuth provider's.

use std::{ffi::OsString, io, path::Path};

use super::{ConfigError, Environment, Problem, Settings, web};
use crate::{outbound::OutboundConfig, types::Secret};

const TEST_ORIGIN_EXPECTED: &str = "an http origin on a loopback IP address, such as \
     http://127.0.0.1:4000";

const TEST_ORIGIN_ONLY_IN_TESTS: &str = "unset outside the test environment (SEC-10)";

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// The settings of the outbound clients: `OUTBOUND_TEST_ORIGIN`, which only the test
    /// environment may set, to an `http` origin on a loopback address (SEC-10).
    pub(super) fn outbound(&self, environment: Environment) -> Result<OutboundConfig, ConfigError> {
        let name = "OUTBOUND_TEST_ORIGIN";
        let test_origin = self.optional_with(name, TEST_ORIGIN_EXPECTED, web::loopback_origin)?;
        if test_origin.is_some() && environment != Environment::Test {
            return Err(ConfigError::new(
                name,
                Problem::Invalid(TEST_ORIGIN_ONLY_IN_TESTS),
            ));
        }
        Ok(OutboundConfig { test_origin })
    }

    /// The setting `name` as text, or `None` when it is unset, for a module that reads a setting
    /// of its own, such as an OAuth provider's client ID.
    ///
    /// # Errors
    ///
    /// When the setting isn't valid Unicode, or names a file that can't be read.
    pub fn text_setting(&self, name: &str) -> Result<Option<String>, ConfigError> {
        self.value(name)
    }

    /// The secret setting `name`, read from the variable or from the file that `<NAME>_FILE`
    /// names (SEC-01), or `None` when it is unset.
    ///
    /// # Errors
    ///
    /// When the setting isn't valid Unicode, or names a file that can't be read.
    pub fn secret_setting(&self, name: &str) -> Result<Option<Secret<String>>, ConfigError> {
        Ok(self.value(name)?.map(Secret::new))
    }
}
