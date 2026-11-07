//! A setting that is missing or invalid, named with what is wrong with it, and never its value,
//! which may be a secret.

/// A setting that is missing or invalid.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("{setting} {problem}")]
pub struct ConfigError {
    setting: String,
    problem: Problem,
}

/// What is wrong with a setting.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum Problem {
    #[error("is required")]
    Missing,
    #[error("must be {0}")]
    Invalid(&'static str),
    #[error("must be a whole number from {0} to {1}")]
    OutOfRange(u32, u32),
    #[error("names a file that can't be read")]
    UnreadableFile,
    #[error("must be valid Unicode")]
    NotUnicode,
    #[error("must be unset: connections take their settings only from the database URLs")]
    MustBeUnset,
    #[error("must be unset: telemetry takes its settings only from OTEL_EXPORTER_OTLP_ENDPOINT")]
    TelemetryMustBeUnset,
}

impl ConfigError {
    pub(super) fn new(setting: &str, problem: Problem) -> Self {
        Self {
            setting: setting.to_owned(),
            problem,
        }
    }
}
