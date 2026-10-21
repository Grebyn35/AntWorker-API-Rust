//! Logging setup: JSON lines in production, readable text elsewhere (OBS-01).

use crate::config::Environment;
use tracing_subscriber::EnvFilter;

/// Logging couldn't be set up, because another logger is already installed.
#[derive(Debug, thiserror::Error)]
#[error("a logger is already installed")]
pub struct LoggingError;

/// Installs the process-wide logger. `RUST_LOG` filters it, and defaults to `info`.
pub fn init_logging(environment: Environment) -> Result<(), LoggingError> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let result = match environment {
        Environment::Production => builder.json().try_init(),
        Environment::Development | Environment::Test => builder.try_init(),
    };
    result.map_err(|_| LoggingError)
}
