//! Typed configuration, loaded once at startup (OPS-01).
//!
//! A setting is read from the environment variable of its name, or from the file that
//! `<NAME>_FILE` names (SEC-01). Values are trimmed, an empty value counts as unset, and a value
//! that isn't valid Unicode is an error.
//! Errors name the setting and the problem, and never the value, which may be a secret.
//! Each subcommand reads only the settings it uses.

use std::{
    ffi::OsString, io, net::SocketAddr, ops::RangeInclusive, path::Path, str::FromStr,
    time::Duration,
};

mod error;
mod mail;
mod web;

pub use error::ConfigError;
use error::Problem;

use ipnet::IpNet;

pub use web::Origin;

use crate::{db::DatabaseUrl, hashing::HmacKey, mail::MailConfig, telemetry::OtlpEndpoint};

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

/// The most detailed log level written (OBS-03). It is never coarser than `INFO`, because
/// every request writes its completion line at `INFO` (PLATFORM-034).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    /// Errors, warnings, lifecycle events and one line per request.
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
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(()),
        }
    }
}

/// The settings every subcommand that logs reads, for its logging and the export of its
/// telemetry.
#[derive(Clone, Debug)]
pub struct TelemetryConfig {
    /// Where the API runs: production logs JSON lines, other environments readable text.
    pub environment: Environment,
    /// The most detailed level written.
    pub level: LogLevel,
    /// Where spans, log records and metrics are exported over OTLP, or `None` to export nothing.
    pub otlp_endpoint: Option<OtlpEndpoint>,
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
    /// The most requests served at once, health probes aside (OPS-07).
    pub max_concurrent_requests: u32,
    /// How long `serve` keeps serving after a shutdown signal (OPS-06).
    pub shutdown_delay: Duration,
    /// How long `serve` waits for the requests in flight once it stops accepting connections.
    pub shutdown_drain: Duration,
    /// The key of every keyed hash, such as a rate limit's bucket key (SEC-03).
    pub hmac_key: HmacKey,
    /// The proxies whose `X-Forwarded-For` names the client (PLATFORM-047).
    pub trusted_proxies: Vec<IpNet>,
    /// The origins of web clients (PLATFORM-052).
    pub cors_allowed_origins: Vec<Origin>,
    /// The IP and user limits (PLATFORM-048).
    pub rate_limits: RateLimitConfig,
    /// How long a dead-lettered job keeps its payload before the sweep deletes it
    /// (PLATFORM-013).
    pub job_dead_letter_retention: Duration,
    /// Whether email is sent, and through which server (MAIL-006).
    pub mail: MailConfig,
}

/// The limits on the requests of one client IP, and of one user (PLATFORM-048).
#[derive(Clone, Copy, Debug)]
pub struct RateLimitConfig {
    /// How many requests one client IP may send in a window.
    pub ip_requests: u32,
    /// How many requests one user may send in a window.
    pub user_requests: u32,
    /// The window's length.
    pub window: Duration,
}

/// The settings `health` reads.
#[derive(Clone, Debug)]
pub struct HealthConfig {
    /// The address the instance's `serve` binds.
    pub listen_addr: SocketAddr,
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

const LISTEN_ADDR_EXPECTED: &str = "a socket address such as 0.0.0.0:8080";

const HMAC_KEY_EXPECTED: &str = "base64 of at least 32 random bytes";

const PROXIES_EXPECTED: &str = "IP addresses and ranges separated by commas, such as \
     172.18.0.0/16, 203.0.113.7";

const ORIGINS_EXPECTED: &str = "origins separated by commas, such as https://app.example.com, \
     each https, or http on a loopback host in development, with no path";

const DATABASE_URL_EXPECTED: &str = "a PostgreSQL URL with a user, a password, a host and a \
     database, and no query parameters but sslmode, sslrootcert, sslcert, sslkey and \
     application_name";

/// The environment variables of libpq that the Postgres driver reads, and that would change a
/// connection whose URL leaves them out. The URL always sets the host, user, password and
/// database, so the variables for those can't change anything. A URL may leave out the port,
/// and with `PGPORT` unset, the driver uses 5432.
const LIBPQ_VARIABLES: [&str; 7] = [
    "PGPORT",
    "PGSSLMODE",
    "PGSSLROOTCERT",
    "PGSSLCERT",
    "PGSSLKEY",
    "PGAPPNAME",
    "PGOPTIONS",
];

const OTLP_ENDPOINT_EXPECTED: &str = "an http or https URL with a host, and no user, password, \
     query or fragment";

/// The environment variables that OpenTelemetry's SDK and its OTLP exporter read on their own,
/// other than `OTEL_EXPORTER_OTLP_ENDPOINT`, which `platform::config` reads. Each would change
/// what the API exports, such as by adding headers or resource attributes, or by replacing the
/// sampler.
const OTEL_VARIABLES: [&str; 42] = [
    "OTEL_BLRP_EXPORT_TIMEOUT",
    "OTEL_BLRP_MAX_EXPORT_BATCH_SIZE",
    "OTEL_BLRP_MAX_QUEUE_SIZE",
    "OTEL_BLRP_SCHEDULE_DELAY",
    "OTEL_BSP_EXPORT_TIMEOUT",
    "OTEL_BSP_MAX_CONCURRENT_EXPORTS",
    "OTEL_BSP_MAX_EXPORT_BATCH_SIZE",
    "OTEL_BSP_MAX_QUEUE_SIZE",
    "OTEL_BSP_SCHEDULE_DELAY",
    "OTEL_METRIC_EXPORT_INTERVAL",
    "OTEL_METRIC_EXPORT_TIMEOUT",
    "OTEL_RESOURCE_ATTRIBUTES",
    "OTEL_SERVICE_NAME",
    "OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT",
    "OTEL_SPAN_EVENT_COUNT_LIMIT",
    "OTEL_SPAN_LINK_COUNT_LIMIT",
    "OTEL_TRACES_SAMPLER",
    "OTEL_TRACES_SAMPLER_ARG",
    "OTEL_EXPORTER_OTLP_COMPRESSION",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "OTEL_EXPORTER_OTLP_INSECURE",
    "OTEL_EXPORTER_OTLP_PROTOCOL",
    "OTEL_EXPORTER_OTLP_TIMEOUT",
    "OTEL_EXPORTER_OTLP_LOGS_COMPRESSION",
    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
    "OTEL_EXPORTER_OTLP_LOGS_INSECURE",
    "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
    "OTEL_EXPORTER_OTLP_LOGS_TIMEOUT",
    "OTEL_EXPORTER_OTLP_METRICS_COMPRESSION",
    "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
    "OTEL_EXPORTER_OTLP_METRICS_HEADERS",
    "OTEL_EXPORTER_OTLP_METRICS_INSECURE",
    "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
    "OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE",
    "OTEL_EXPORTER_OTLP_METRICS_TIMEOUT",
    "OTEL_EXPORTER_OTLP_TRACES_COMPRESSION",
    "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
    "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
    "OTEL_EXPORTER_OTLP_TRACES_INSECURE",
    "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
    "OTEL_EXPORTER_OTLP_TRACES_TIMEOUT",
];

/// Reads settings from a source of environment variables and the files they name.
pub struct Settings<E, F> {
    env: E,
    read_file: F,
}

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// Settings read through `env`, with `read_file` for `<NAME>_FILE` variables.
    pub fn new(env: E, read_file: F) -> Self {
        Self { env, read_file }
    }

    /// The settings for logging and telemetry. Every OpenTelemetry variable that the SDK would
    /// read on its own must be unset, so these settings are the only source of telemetry's. A
    /// variable set to an empty value counts as set.
    pub fn telemetry(&self) -> Result<TelemetryConfig, ConfigError> {
        if let Some(variable) = OTEL_VARIABLES
            .iter()
            .find(|variable| (self.env)(variable).is_some())
        {
            return Err(ConfigError::new(variable, Problem::TelemetryMustBeUnset));
        }
        Ok(TelemetryConfig {
            environment: self.required("ENVIRONMENT", "development, test or production")?,
            level: self.optional("LOG_LEVEL", LogLevel::Info, "info, debug or trace")?,
            otlp_endpoint: self.parsed("OTEL_EXPORTER_OTLP_ENDPOINT", OTLP_ENDPOINT_EXPECTED)?,
        })
    }

    /// The settings for `serve`.
    pub fn server(&self) -> Result<ServerConfig, ConfigError> {
        Ok(ServerConfig {
            listen_addr: self.required("LISTEN_ADDR", LISTEN_ADDR_EXPECTED)?,
            database_url: self.database_url("DATABASE_URL")?,
            database_max_connections: self.number("DATABASE_MAX_CONNECTIONS", 10, 1..=1000)?,
            max_concurrent_requests: self.number("MAX_CONCURRENT_REQUESTS", 100, 1..=100_000)?,
            shutdown_delay: self.seconds("SHUTDOWN_DELAY_SECONDS", 5)?,
            shutdown_drain: self.seconds("SHUTDOWN_DRAIN_SECONDS", 10)?,
            hmac_key: self.with("HMAC_KEY", HMAC_KEY_EXPECTED, web::hmac_key)?,
            trusted_proxies: self
                .optional_with("TRUSTED_PROXIES", PROXIES_EXPECTED, web::proxies)?
                .unwrap_or_default(),
            cors_allowed_origins: self.origins()?,
            rate_limits: RateLimitConfig {
                ip_requests: self.number("RATE_LIMIT_IP_REQUESTS", 600, 1..=1_000_000)?,
                user_requests: self.number("RATE_LIMIT_USER_REQUESTS", 300, 1..=1_000_000)?,
                window: self.duration("RATE_LIMIT_WINDOW_SECONDS", 60, 1..=86_400)?,
            },
            job_dead_letter_retention: Duration::from_secs(
                u64::from(self.number("JOB_DEAD_LETTER_RETENTION_DAYS", 14, 1..=365)?) * 86_400,
            ),
            mail: self.mail()?,
        })
    }

    /// A whole number from the setting `name`, or `default` when it is unset (OPS-01).
    pub fn number(
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

    /// A duration in whole seconds from the setting `name`, or `default` seconds when it is
    /// unset.
    pub fn duration(
        &self,
        name: &str,
        default: u32,
        range: RangeInclusive<u32>,
    ) -> Result<Duration, ConfigError> {
        Ok(Duration::from_secs(
            self.number(name, default, range)?.into(),
        ))
    }

    /// The origins of `CORS_ALLOWED_ORIGINS`, which `ENVIRONMENT` decides whether plain `http`
    /// may name.
    fn origins(&self) -> Result<Vec<Origin>, ConfigError> {
        let environment = self.required("ENVIRONMENT", "development, test or production")?;
        Ok(self
            .optional_with("CORS_ALLOWED_ORIGINS", ORIGINS_EXPECTED, |value| {
                web::origins(value, environment)
            })?
            .unwrap_or_default())
    }

    /// The required setting `name`, as `parse` reads it.
    fn with<T>(
        &self,
        name: &str,
        expected: &'static str,
        parse: impl Fn(&str) -> Option<T>,
    ) -> Result<T, ConfigError> {
        self.optional_with(name, expected, parse)?
            .ok_or_else(|| ConfigError::new(name, Problem::Missing))
    }

    /// The setting `name`, as `parse` reads it, or `None` when it is unset.
    fn optional_with<T>(
        &self,
        name: &str,
        expected: &'static str,
        parse: impl Fn(&str) -> Option<T>,
    ) -> Result<Option<T>, ConfigError> {
        self.value(name)?
            .map(|value| {
                parse(&value).ok_or_else(|| ConfigError::new(name, Problem::Invalid(expected)))
            })
            .transpose()
    }

    /// The settings for `health`.
    pub fn health(&self) -> Result<HealthConfig, ConfigError> {
        Ok(HealthConfig {
            listen_addr: self.required("LISTEN_ADDR", LISTEN_ADDR_EXPECTED)?,
        })
    }

    /// The settings for `migrate`.
    pub fn migrate(&self) -> Result<MigrateConfig, ConfigError> {
        Ok(MigrateConfig {
            owner_url: self.database_url("DATABASE_OWNER_URL")?,
        })
    }

    /// The settings for `bootstrap`.
    pub fn bootstrap(&self) -> Result<BootstrapConfig, ConfigError> {
        Ok(BootstrapConfig {
            admin: self.database_url("DATABASE_ADMIN_URL")?,
            owner: self.database_url("DATABASE_OWNER_URL")?,
            app: self.database_url("DATABASE_URL")?,
        })
    }

    /// The database URL in the setting `name`. Every libpq variable that would change its
    /// connections must be unset, so the URL is the only source of connection settings. The
    /// driver reads a variable that is set to an empty value too, so any value counts as set.
    fn database_url(&self, name: &str) -> Result<DatabaseUrl, ConfigError> {
        if let Some(variable) = LIBPQ_VARIABLES
            .iter()
            .find(|variable| (self.env)(variable).is_some())
        {
            return Err(ConfigError::new(variable, Problem::MustBeUnset));
        }
        self.required(name, DATABASE_URL_EXPECTED)
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

    /// A duration in whole seconds, from 0 to 300.
    fn seconds(&self, name: &str, default: u32) -> Result<Duration, ConfigError> {
        self.duration(name, default, 0..=300)
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
        let raw = match non_empty(self.text(&file_setting)?) {
            Some(path) => Some(
                (self.read_file)(Path::new(&path))
                    .map_err(|_| ConfigError::new(&file_setting, Problem::UnreadableFile))?,
            ),
            None => self.text(name)?,
        };
        Ok(non_empty(raw))
    }

    /// The variable `name` as text. A value that isn't valid Unicode is an error, so it can't
    /// pass for an unset one.
    fn text(&self, name: &str) -> Result<Option<String>, ConfigError> {
        (self.env)(name)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| ConfigError::new(name, Problem::NotUnicode))
            })
            .transpose()
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Settings from the process environment and the files that `<NAME>_FILE` variables name.
/// Reading a setting, such as through [`Settings::server`], may read its file, so read the
/// settings before the async runtime starts.
pub fn from_process()
-> Settings<impl Fn(&str) -> Option<OsString>, impl Fn(&Path) -> io::Result<String>> {
    #[expect(
        clippy::disallowed_methods,
        reason = "configuration is read before the async runtime starts"
    )]
    fn read_file(path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
    Settings::new(|name| std::env::var_os(name), read_file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const SERVER: [(&str, &str); 5] = [
        ("ENVIRONMENT", "test"),
        ("LISTEN_ADDR", "0.0.0.0:8080"),
        ("DATABASE_URL", "postgres://app:secret@localhost:5432/api"),
        ("HMAC_KEY", "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc="),
        ("EMAIL", "off"),
    ];

    fn settings(
        vars: &[(&str, &str)],
        files: &[(&str, &str)],
    ) -> Settings<impl Fn(&str) -> Option<OsString>, impl Fn(&Path) -> io::Result<String>> {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(*v)))
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
            .telemetry()
            .unwrap();
        assert_eq!(logging.environment, Environment::Production);
    }

    #[test]
    fn platform_023_an_empty_value_counts_as_unset() {
        let error = settings(&[("ENVIRONMENT", "   ")], &[])
            .telemetry()
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
            &with(&SERVER, &[("DATABASE_URL", "postgres://app@localhost/api")]),
            &[],
        )
        .server()
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("DATABASE_URL must be {DATABASE_URL_EXPECTED}")
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
            &[("/run/secrets/db", "postgres://app:secret@db:5432/api\n")],
        )
        .server()
        .unwrap();
        assert_eq!(config.database_url.database(), "api");
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
    fn ops_01_the_log_level_is_info_or_finer() {
        for level in ["warn", "error"] {
            let error = settings(&[("ENVIRONMENT", "test"), ("LOG_LEVEL", level)], &[])
                .telemetry()
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "LOG_LEVEL must be info, debug or trace",
                "{level}"
            );
        }
    }

    #[test]
    fn ops_01_an_optional_setting_has_its_default() {
        let telemetry = settings(&[("ENVIRONMENT", "test")], &[])
            .telemetry()
            .unwrap();
        assert_eq!(telemetry.level, LogLevel::Info);
        assert!(telemetry.otlp_endpoint.is_none());
        let config = settings(&SERVER, &[]).server().unwrap();
        assert_eq!(config.database_max_connections, 10);
        assert_eq!(config.max_concurrent_requests, 100);
        assert_eq!(config.shutdown_delay, Duration::from_secs(5));
        assert_eq!(config.shutdown_drain, Duration::from_secs(10));
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
    fn ops_01_the_server_s_limits_are_range_checked() {
        for (name, value, range) in [
            ("MAX_CONCURRENT_REQUESTS", "0", "1 to 100000"),
            ("MAX_CONCURRENT_REQUESTS", "100001", "1 to 100000"),
            ("SHUTDOWN_DELAY_SECONDS", "301", "0 to 300"),
            ("SHUTDOWN_DRAIN_SECONDS", "-1", "0 to 300"),
        ] {
            let error = settings(&with(&SERVER, &[(name, value)]), &[])
                .server()
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("{name} must be a whole number from {range}")
            );
        }
        let config = settings(
            &with(
                &SERVER,
                &[
                    ("SHUTDOWN_DELAY_SECONDS", "0"),
                    ("SHUTDOWN_DRAIN_SECONDS", "300"),
                ],
            ),
            &[],
        )
        .server()
        .unwrap();
        assert_eq!(config.shutdown_delay, Duration::ZERO);
        assert_eq!(config.shutdown_drain, Duration::from_secs(300));
    }

    #[test]
    fn ops_01_each_subcommand_reads_only_its_settings() {
        let migrate = settings(
            &[(
                "DATABASE_OWNER_URL",
                "postgres://owner:secret@localhost:5432/api",
            )],
            &[],
        )
        .migrate()
        .unwrap();
        assert_eq!(migrate.owner_url.database(), "api");
        let error = settings(&SERVER, &[]).bootstrap().unwrap_err();
        assert_eq!(error.to_string(), "DATABASE_ADMIN_URL is required");
    }

    #[test]
    fn ops_01_libpq_variables_that_would_change_connections_must_be_unset() {
        for name in [
            "PGPORT",
            "PGSSLMODE",
            "PGSSLROOTCERT",
            "PGSSLCERT",
            "PGSSLKEY",
            "PGAPPNAME",
            "PGOPTIONS",
        ] {
            // The driver reads a variable that is set to an empty value, too.
            for value in ["x", "", "  "] {
                let error = settings(&with(&SERVER, &[(name, value)]), &[])
                    .server()
                    .unwrap_err();
                assert_eq!(
                    error.to_string(),
                    format!(
                        "{name} must be unset: connections take their settings only from the database URLs"
                    ),
                    "{name}={value:?}"
                );
            }
        }
        let migrate = settings(
            &[
                (
                    "DATABASE_OWNER_URL",
                    "postgres://owner:secret@localhost:5432/api",
                ),
                ("PGOPTIONS", "-c statement_timeout=0"),
            ],
            &[],
        )
        .migrate();
        assert!(migrate.is_err());
        let overridden = settings(&with(&SERVER, &[("PGHOST", "elsewhere")]), &[]).server();
        assert!(overridden.is_ok(), "the URL always sets the host");
    }

    #[test]
    fn ops_01_the_certificate_variables_of_the_tls_library_are_left_to_it() {
        for name in ["SSL_CERT_FILE", "SSL_CERT_DIR"] {
            let config = settings(&with(&SERVER, &[(name, "/etc/certs")]), &[]).server();

            assert!(config.is_ok(), "{name}: {:?}", config.err());
        }
    }

    #[test]
    fn ops_02_env_example_states_each_settings_type_and_default() {
        let example = include_str!("../../../../.env.example");
        let mut comments: Vec<&str> = Vec::new();
        for line in example.lines() {
            if let Some(comment) = line.strip_prefix('#') {
                comments.push(comment);
                continue;
            }
            if let Some((name, _)) = line.split_once('=') {
                let text = comments.join(" ");
                assert!(text.contains("Type:"), "{name} states no type");
                assert!(text.contains("default"), "{name} states no default");
            }
            comments.clear();
        }
    }

    #[test]
    fn ops_02_env_example_names_every_libpq_variable_that_must_be_unset() {
        let example = include_str!("../../../../.env.example");
        for name in LIBPQ_VARIABLES {
            assert!(example.contains(name), "{name}");
        }
    }

    #[test]
    fn platform_041_the_sdk_s_own_variables_must_be_unset() {
        for name in OTEL_VARIABLES {
            for value in ["x", "", "  "] {
                let error = settings(&[("ENVIRONMENT", "test"), (name, value)], &[])
                    .telemetry()
                    .unwrap_err();
                assert_eq!(
                    error.to_string(),
                    format!(
                        "{name} must be unset: telemetry takes its settings only from OTEL_EXPORTER_OTLP_ENDPOINT"
                    ),
                    "{name}={value:?}"
                );
            }
        }
    }

    #[test]
    fn platform_041_env_example_names_every_variable_that_must_be_unset() {
        let example = include_str!("../../../../.env.example");
        let words: Vec<&str> = example
            .split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .collect();
        for name in OTEL_VARIABLES {
            assert!(words.contains(&name), "{name}");
        }
    }

    #[test]
    fn platform_041_every_variable_env_example_lists_is_refused() {
        let example = include_str!("../../../../.env.example");
        let listed: Vec<&str> = example
            .split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .filter(|word| word.starts_with("OTEL_") && *word != "OTEL_EXPORTER_OTLP_ENDPOINT")
            .collect();
        assert_eq!(listed.len(), OTEL_VARIABLES.len());
        for name in listed {
            assert!(OTEL_VARIABLES.contains(&name), "{name}");
        }
    }

    #[test]
    fn platform_039_the_endpoint_is_an_http_or_https_url() {
        for endpoint in ["http://127.0.0.1:4318", "https://collector.internal/otlp/"] {
            let config = settings(
                &[
                    ("ENVIRONMENT", "test"),
                    ("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint),
                ],
                &[],
            )
            .telemetry()
            .unwrap();
            assert!(config.otlp_endpoint.is_some(), "{endpoint}");
        }
    }

    #[test]
    fn obs_10_an_endpoint_the_exporters_can_t_use_fails_startup() {
        for endpoint in [
            "collector:4318",
            "ftp://collector:4318",
            "http://",
            "http://user:secret@collector:4318",
            "http://collector:4318?token=secret",
            "http://collector:4318#traces",
        ] {
            let error = settings(
                &[
                    ("ENVIRONMENT", "test"),
                    ("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint),
                ],
                &[],
            )
            .telemetry()
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("OTEL_EXPORTER_OTLP_ENDPOINT must be {OTLP_ENDPOINT_EXPECTED}"),
                "{endpoint}"
            );
            assert!(!error.to_string().contains("secret"));
        }
    }
}
