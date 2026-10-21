//! Typed configuration, loaded once at startup (OPS-01).
//!
//! A setting is read from the environment variable of its name, or from the file that
//! `<NAME>_FILE` names (SEC-01). Values are trimmed, and an empty value counts as unset.
//! Errors name the setting and the problem, and never the value, which may be a secret.

use std::{io, net::SocketAddr, path::Path, str::FromStr};

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

/// The settings the `serve` subcommand needs.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Where the API runs.
    pub environment: Environment,
    /// The address the HTTP server binds.
    pub listen_addr: SocketAddr,
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

    /// The settings for `serve`.
    pub fn server(&self) -> Result<ServerConfig, ConfigError> {
        Ok(ServerConfig {
            environment: self.required("ENVIRONMENT", "development, test or production")?,
            listen_addr: self.required("LISTEN_ADDR", "a socket address such as 0.0.0.0:8080")?,
        })
    }

    fn required<T: FromStr>(&self, name: &str, expected: &'static str) -> Result<T, ConfigError> {
        let value = self
            .value(name)?
            .ok_or_else(|| ConfigError::new(name, Problem::Missing))?;
        value
            .parse()
            .map_err(|_| ConfigError::new(name, Problem::Invalid(expected)))
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

    #[test]
    fn platform_023_values_are_trimmed() {
        let config = settings(
            &[
                ("ENVIRONMENT", "  production "),
                ("LISTEN_ADDR", " 0.0.0.0:8080\n"),
            ],
            &[],
        )
        .server()
        .unwrap();
        assert_eq!(config.environment, Environment::Production);
        assert_eq!(config.listen_addr.to_string(), "0.0.0.0:8080");
    }

    #[test]
    fn platform_023_an_empty_value_counts_as_unset() {
        let error = settings(
            &[("ENVIRONMENT", "   "), ("LISTEN_ADDR", "0.0.0.0:8080")],
            &[],
        )
        .server()
        .unwrap_err();
        assert_eq!(error.to_string(), "ENVIRONMENT is required");
    }

    #[test]
    fn platform_023_a_missing_required_setting_is_an_error() {
        let error = settings(&[("ENVIRONMENT", "test")], &[])
            .server()
            .unwrap_err();
        assert_eq!(error.to_string(), "LISTEN_ADDR is required");
    }

    #[test]
    fn platform_023_an_invalid_value_names_the_setting_and_not_the_value() {
        let error = settings(
            &[("ENVIRONMENT", "staging"), ("LISTEN_ADDR", "0.0.0.0:8080")],
            &[],
        )
        .server()
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "ENVIRONMENT must be development, test or production"
        );
        assert!(!error.to_string().contains("staging"));
    }

    #[test]
    fn platform_023_a_file_setting_is_read_and_trimmed() {
        let config = settings(
            &[
                ("ENVIRONMENT", "development"),
                ("LISTEN_ADDR_FILE", "/run/secrets/addr"),
            ],
            &[("/run/secrets/addr", "127.0.0.1:9000\n")],
        )
        .server()
        .unwrap();
        assert_eq!(config.listen_addr.to_string(), "127.0.0.1:9000");
    }

    #[test]
    fn platform_023_an_unreadable_file_names_the_file_setting() {
        let error = settings(
            &[
                ("ENVIRONMENT", "development"),
                ("LISTEN_ADDR_FILE", "/missing"),
            ],
            &[],
        )
        .server()
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "LISTEN_ADDR_FILE names a file that can't be read"
        );
    }

    #[test]
    fn platform_023_environment_is_case_insensitive_but_closed() {
        assert_eq!("Production".parse(), Ok(Environment::Production));
        assert_eq!("prod".parse::<Environment>(), Err(()));
    }
}
