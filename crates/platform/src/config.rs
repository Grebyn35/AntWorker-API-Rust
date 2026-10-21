//! Typed configuration, loaded once at startup (OPS-01).
//!
//! A setting is read from the environment variable of its name, or from the file that
//! `<NAME>_FILE` names (SEC-01). Values are trimmed, and an empty value counts as unset.
//! Errors name the setting and the problem, and never the value, which may be a secret.
//! Each subcommand reads only the settings it uses.

use std::{io, net::SocketAddr, ops::RangeInclusive, path::Path, str::FromStr};

use crate::db::DatabaseUrl;

/// Where the API runs. Every deployment sets it explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Environment {
    /// A developer's machine.
    Development,
    /// An automated test run.
    Test,
    /// A real deployment.
    Production,
}

impl FromStr for Environment {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "development" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "production" => Ok(Self::Production),
            _ => Err(()),
        }
    }
}

/// The most detailed log level written (OBS-03).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    /// Only events a human must look at.
    Error,
    /// Also degraded but handled events.
    Warn,
    /// Also lifecycle events and one line per request.
    Info,
    /// Also diagnostics.
    Debug,
    /// Everything.
    Trace,
}

impl FromStr for LogLevel {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(()),
        }
    }
}

/// The settings every subcommand reads, for its logging.
#[derive(Clone, Copy, Debug)]
pub struct LoggingConfig {
    /// Where the API runs: production logs JSON lines, other environments readable text.
    pub environment: Environment,
    /// The most detailed level written.
    pub level: LogLevel,
}

/// The settings `serve` reads.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// The address the HTTP server binds.
    pub listen_addr: SocketAddr,
    /// The connection the API uses, as the role `app`.
    pub database_url: DatabaseUrl,
    /// The most connections the pool opens.
    pub database_max_connections: u32,
}

/// The settings `migrate` reads.
#[derive(Clone, Debug)]
pub struct MigrateConfig {
    /// The connection migrations run on, as the role `owner`.
    pub owner_url: DatabaseUrl,
}

/// The settings `bootstrap` reads.
#[derive(Clone, Debug)]
pub struct BootstrapConfig {
    /// An administrator's connection to the database.
    pub admin: DatabaseUrl,
    /// The role `owner`'s connection, whose password `bootstrap` sets.
    pub owner: DatabaseUrl,
    /// The role `app`'s connection, whose password `bootstrap` sets.
    pub app: DatabaseUrl,
}

/// A setting that is missing or invalid.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{setting} {problem}")]
pub struct ConfigError {
    setting: String,
    problem: Problem,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum Problem {
    #[error("is required")]
    Missing,
    #[error("must be {0}")]
    Invalid(&'static str),
    #[error("must be a whole number from {0} to {1}")]
    OutOfRange(u32, u32),
    #[error("names a file that can't be read")]
    UnreadableFile,
}

impl ConfigError {
    fn new(setting: &str, problem: Problem) -> Self {
        Self {
            setting: setting.to_owned(),
            problem,
        }
    }
}

const DATABASE_URL_EXPECTED: &str =
    "a PostgreSQL URL with a user, a password, a host and a database";

/// Reads settings from a source of environment variables and the files they name.
pub struct Settings<E, F> {
    env: E,
    read_file: F,
}

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<String>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// Settings read through `env`, with `read_file` for `<NAME>_FILE` variables.
    pub fn new(env: E, read_file: F) -> Self {
        Self { env, read_file }
    }

    /// The settings for logging.
    pub fn logging(&self) -> Result<LoggingConfig, ConfigError> {
        Ok(LoggingConfig {
            environment: self.required("ENVIRONMENT", "development, test or production")?,
            level: self.optional(
                "LOG_LEVEL",
                LogLevel::Info,
                "error, warn, info, debug or trace",
            )?,
        })
    }

    /// The settings for `serve`.
    pub fn server(&self) -> Result<ServerConfig, ConfigError> {
        Ok(ServerConfig {
            listen_addr: self.required("LISTEN_ADDR", "a socket address such as 0.0.0.0:8080")?,
            database_url: self.required("DATABASE_URL", DATABASE_URL_EXPECTED)?,
            database_max_connections: self.number("DATABASE_MAX_CONNECTIONS", 10, 1..=1000)?,
        })
    }

    /// The settings for `migrate`.
    pub fn migrate(&self) -> Result<MigrateConfig, ConfigError> {
        Ok(MigrateConfig {
            owner_url: self.required("DATABASE_OWNER_URL", DATABASE_URL_EXPECTED)?,
        })
    }

    /// The settings for `bootstrap`.
    pub fn bootstrap(&self) -> Result<BootstrapConfig, ConfigError> {
        Ok(BootstrapConfig {
            admin: self.required("DATABASE_ADMIN_URL", DATABASE_URL_EXPECTED)?,
            owner: self.required("DATABASE_OWNER_URL", DATABASE_URL_EXPECTED)?,
            app: self.required("DATABASE_URL", DATABASE_URL_EXPECTED)?,
        })
    }

    fn required<T: FromStr>(&self, name: &str, expected: &'static str) -> Result<T, ConfigError> {
        self.parsed(name, expected)?
            .ok_or_else(|| ConfigError::new(name, Problem::Missing))
    }

    fn optional<T: FromStr>(
        &self,
        name: &str,
        default: T,
        expected: &'static str,
    ) -> Result<T, ConfigError> {
        Ok(self.parsed(name, expected)?.unwrap_or(default))
    }

    fn number(
        &self,
        name: &str,
        default: u32,
        range: RangeInclusive<u32>,
    ) -> Result<u32, ConfigError> {
        let Some(value) = self.value(name)? else {
            return Ok(default);
        };
        value
            .parse()
            .ok()
            .filter(|number| range.contains(number))
            .ok_or_else(|| {
                ConfigError::new(name, Problem::OutOfRange(*range.start(), *range.end()))
            })
    }

    fn parsed<T: FromStr>(
        &self,
        name: &str,
        expected: &'static str,
    ) -> Result<Option<T>, ConfigError> {
        self.value(name)?
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| ConfigError::new(name, Problem::Invalid(expected)))
            })
            .transpose()
    }

    fn value(&self, name: &str) -> Result<Option<String>, ConfigError> {
        let file_setting = format!("{name}_FILE");
        let raw = match non_empty((self.env)(&file_setting)) {
            Some(path) => Some(
                (self.read_file)(Path::new(&path))
                    .map_err(|_| ConfigError::new(&file_setting, Problem::UnreadableFile))?,
            ),
            None => (self.env)(name),
        };
        Ok(non_empty(raw))
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Settings from the process environment and the filesystem. Call it before the async
/// runtime starts, because it may read files.
pub fn from_process()
-> Settings<impl Fn(&str) -> Option<String>, impl Fn(&Path) -> io::Result<String>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "configuration is read before the async runtime starts"
    )]
    fn read_file(path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
    Settings::new(|name| std::env::var(name).ok(), read_file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SERVER: [(&str, &str); 2] = [
        ("LISTEN_ADDR", "0.0.0.0:8080"),
        (
            "DATABASE_URL",
            "postgres://app:secret@localhost:5432/antworker",
        ),
    ];

    fn settings(
        vars: &[(&str, &str)],
        files: &[(&str, &str)],
    ) -> Settings<impl Fn(&str) -> Option<String>, impl Fn(&Path) -> io::Result<String>> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let files: HashMap<String, String> = files
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Settings::new(
            move |name| vars.get(name).cloned(),
            move |path| {
                files
                    .get(path.to_string_lossy().as_ref())
                    .cloned()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
            },
        )
    }

    fn with(
        base: &[(&'static str, &'static str)],
        extra: &[(&'static str, &'static str)],
    ) -> Vec<(&'static str, &'static str)> {
        let mut vars: Vec<_> = base
            .iter()
            .filter(|(k, _)| !extra.iter().any(|(e, _)| e == k))
            .copied()
            .collect();
        vars.extend_from_slice(extra);
        vars
    }

    #[test]
    fn platform_023_values_are_trimmed() {
        let config = settings(&with(&SERVER, &[("LISTEN_ADDR", " 0.0.0.0:8080\n")]), &[])
            .server()
            .unwrap();
        assert_eq!(config.listen_addr.to_string(), "0.0.0.0:8080");
        let logging = settings(&[("ENVIRONMENT", "  production ")], &[])
            .logging()
            .unwrap();
        assert_eq!(logging.environment, Environment::Production);
    }

    #[test]
    fn platform_023_an_empty_value_counts_as_unset() {
        let error = settings(&[("ENVIRONMENT", "   ")], &[])
            .logging()
            .unwrap_err();
        assert_eq!(error.to_string(), "ENVIRONMENT is required");
    }

    #[test]
    fn platform_023_a_missing_required_setting_is_an_error() {
        let error = settings(&with(&SERVER, &[("DATABASE_URL", "")]), &[])
            .server()
            .unwrap_err();
        assert_eq!(error.to_string(), "DATABASE_URL is required");
    }

    #[test]
    fn platform_023_an_invalid_value_names_the_setting_and_not_the_value() {
        let error = settings(
            &with(
                &SERVER,
                &[("DATABASE_URL", "postgres://app@localhost/antworker")],
            ),
            &[],
        )
        .server()
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "DATABASE_URL must be a PostgreSQL URL with a user, a password, a host and a database"
        );
        assert!(!error.to_string().contains("localhost"));
    }

    #[test]
    fn platform_023_a_file_setting_is_read_and_trimmed() {
        let config = settings(
            &with(
                &SERVER,
                &[
                    ("DATABASE_URL", ""),
                    ("DATABASE_URL_FILE", "/run/secrets/db"),
                ],
            ),
            &[(
                "/run/secrets/db",
                "postgres://app:secret@db:5432/antworker\n",
            )],
        )
        .server()
        .unwrap();
        assert_eq!(config.database_url.database(), "antworker");
    }

    #[test]
    fn platform_023_an_unreadable_file_names_the_file_setting() {
        let error = settings(&with(&SERVER, &[("DATABASE_URL_FILE", "/missing")]), &[])
            .server()
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "DATABASE_URL_FILE names a file that can't be read"
        );
    }

    #[test]
    fn ops_01_closed_sets_are_case_insensitive_but_closed() {
        assert_eq!("Production".parse(), Ok(Environment::Production));
        assert_eq!("prod".parse::<Environment>(), Err(()));
        assert_eq!("DEBUG".parse(), Ok(LogLevel::Debug));
        assert_eq!("verbose".parse::<LogLevel>(), Err(()));
    }

    #[test]
    fn ops_01_an_optional_setting_has_its_default() {
        let logging = settings(&[("ENVIRONMENT", "test")], &[]).logging().unwrap();
        assert_eq!(logging.level, LogLevel::Info);
        let config = settings(&SERVER, &[]).server().unwrap();
        assert_eq!(config.database_max_connections, 10);
    }

    #[test]
    fn ops_01_numbers_are_range_checked() {
        for value in ["0", "1001", "-1", "ten"] {
            let error = settings(&with(&SERVER, &[("DATABASE_MAX_CONNECTIONS", value)]), &[])
                .server()
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "DATABASE_MAX_CONNECTIONS must be a whole number from 1 to 1000",
                "{value}"
            );
        }
    }

    #[test]
    fn ops_01_each_subcommand_reads_only_its_settings() {
        let migrate = settings(
            &[(
                "DATABASE_OWNER_URL",
                "postgres://owner:secret@localhost:5432/antworker",
            )],
            &[],
        )
        .migrate()
        .unwrap();
        assert_eq!(migrate.owner_url.database(), "antworker");
        let error = settings(&SERVER, &[]).bootstrap().unwrap_err();
        assert_eq!(error.to_string(), "DATABASE_ADMIN_URL is required");
    }
}
