//! The repository's custom CI checks. Each subcommand runs as a `just` task (OPS-12), and the
//! API crates never depend on this one (ARCH-01).

mod acronyms;
mod alerts;
mod catalog;
mod declarations;
mod docs;
mod formats;
mod instrument;
mod markdown;
mod metrics;
mod openapi;
mod promql;
mod repo;
mod routes;
mod stack;
mod telemetry_config;
mod test_map;
mod text;

use std::{fmt, io, process::ExitCode};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "The repository's custom CI checks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Reports which spec behaviors tests prove, and fails on a test whose name doesn't start
    /// with a behavior or rule ID that exists (TEST-01, DOC-10).
    #[command(name = "check-test-map")]
    TestMap,
    /// Fails on a link, ID or named CI check that doesn't resolve, a history marker, an
    /// unfinished-work marker, a document outside the documentation set, an acronym not spelled
    /// out where a document first uses it, a spec, rule or tracker whose format breaks DOC-15 or
    /// DOC-16, or a spec's Routes table that doesn't match `openapi.json` (DOC-01, DOC-07 to
    /// DOC-09, DOC-13 to DOC-16).
    #[command(name = "check-docs")]
    Docs,
    /// Fails on a part of the committed `openapi.json` that breaks the rules for the HTTP
    /// contract: its paths, its property names, each operation's access level and response
    /// descriptions, the codes of its error responses, and its references, which must resolve
    /// (API-01, API-05, API-17, ERR-02, ERR-13).
    #[command(name = "check-openapi")]
    Openapi,
    /// Fails on a log or span field name outside the list of telemetry names, a record name
    /// outside it, a message that isn't constant, an `#[instrument]` without `skip_all`, an
    /// event at any level but `DEBUG`, or a span, in `app` code other than `main.rs`, and an
    /// exception to a print lint outside the places printing is allowed (CODE-03, OBS-01,
    /// OBS-03 to OBS-06).
    #[command(name = "check-instrument")]
    Instrument,
    /// Fails on a metric declared or named outside OBS-11, a catalog that differs from the
    /// declarations, a dashboard or alert that reads a metric nothing declares or lists, a
    /// metric that nothing reads, or a panel title that isn't a question (OBS-11 to OBS-13).
    #[command(name = "check-metrics")]
    Metrics,
    /// Writes the metric catalog, `docs/operations/metrics.md`, from the declarations (OBS-12).
    #[command(name = "metric-catalog")]
    MetricCatalog,
    /// Fails on a collector configuration whose tail sampling doesn't keep the traces OBS-08
    /// names, or a latency objective that differs between the sampling, the alerts and the
    /// latency dashboard (OBS-08).
    #[command(name = "check-telemetry-config")]
    TelemetryConfig,
    /// Fails on an alert without a severity, a runbook section, or a test that expects it to
    /// fire (OBS-14).
    #[command(name = "alert-rules")]
    AlertRules,
}

/// A problem a check found, at a line of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The file, as a path from the repository root.
    pub path: String,
    /// The line, counted from 1.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl Finding {
    /// A finding at `line` of the file at `path`.
    pub fn new(path: &str, line: usize, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.message)
    }
}

/// What a check found: a report of the repository's state, and the problems that fail it.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Lines that describe the repository, such as which behaviors tests prove.
    pub report: Vec<String>,
    /// The problems. Any problem fails the check.
    pub findings: Vec<Finding>,
}

fn main() -> ExitCode {
    let command = Cli::parse().command;
    let repo = match repo::Repo::load() {
        Ok(repo) => repo,
        Err(error) => return unreadable(&error),
    };
    let catalog = catalog::Catalog::read(&repo);
    let outcome = match command {
        Command::TestMap => test_map::check(&repo, &catalog),
        Command::Docs => docs::check(&repo, &catalog),
        Command::Openapi => openapi::check(&repo),
        Command::Instrument => instrument::check(&repo),
        Command::Metrics => metrics::check(&repo),
        Command::MetricCatalog => write_catalog(&repo),
        Command::TelemetryConfig => telemetry_config::check(&repo),
        Command::AlertRules => alerts::check(&repo),
    };
    print(&outcome)
}

/// Writes the metric catalog, or reports why it can't be written.
fn write_catalog(repo: &repo::Repo) -> Outcome {
    let findings = match metrics::write(repo) {
        Ok(text) => match repo::write(metrics::CATALOG, &text) {
            Ok(()) => Vec::new(),
            Err(error) => vec![Finding::new(metrics::CATALOG, 1, error.to_string())],
        },
        Err(findings) => findings,
    };
    Outcome {
        report: Vec::new(),
        findings,
    }
}

#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the report and the findings are the tool's output"
)]
fn print(outcome: &Outcome) -> ExitCode {
    for line in &outcome.report {
        println!("{line}");
    }
    for finding in &outcome.findings {
        eprintln!("{finding}");
    }
    if outcome.findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[expect(clippy::print_stderr, reason = "the error is the tool's output")]
fn unreadable(error: &io::Error) -> ExitCode {
    eprintln!("can't read the repository: {error}");
    ExitCode::FAILURE
}
