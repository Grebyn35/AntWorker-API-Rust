//! Logging setup: JSON lines in production, readable text elsewhere (OBS-01).

use std::io;

use tracing::Subscriber;
use tracing_subscriber::{filter::LevelFilter, fmt::MakeWriter};

use crate::config::{Environment, LogLevel, LoggingConfig};

/// Logging couldn't be set up, because another logger is already installed.
#[derive(Debug, thiserror::Error)]
#[error("a logger is already installed")]
pub struct LoggingError;

/// Installs the process-wide logger, writing to stdout.
pub fn init_logging(config: &LoggingConfig) -> Result<(), LoggingError> {
    let level = level_filter(config.level);
    let installed = match config.environment {
        Environment::Production => tracing::subscriber::set_global_default(json(level, io::stdout)),
        Environment::Development | Environment::Test => {
            tracing::subscriber::set_global_default(text(level, io::stdout))
        }
    };
    installed.map_err(|_| LoggingError)
}

/// One JSON object per line. An event's fields sit at the top level, next to `timestamp`,
/// `level` and `message`, so every line has the shape of a startup failure line (OPS-03).
pub(crate) fn json<W>(level: LevelFilter, writer: W) -> impl Subscriber + Send + Sync + 'static
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_max_level(level)
        .with_writer(writer)
        .finish()
}

fn text<W>(level: LevelFilter, writer: W) -> impl Subscriber + Send + Sync + 'static
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(writer)
        .finish()
}

fn level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    }
}
