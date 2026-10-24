//! The repository's custom CI checks. Each subcommand runs as a `just` task (OPS-12), and the
//! API crates never depend on this one (ARCH-01).

mod acronyms;
mod catalog;
mod docs;
mod formats;
mod instrument;
mod markdown;
mod openapi;
mod repo;
mod routes;
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
    /// unfinished-work marker, a document outside the documentation set, or a spec, rule or
    /// tracker whose format breaks DOC-15 or DOC-16 (DOC-01, DOC-07, DOC-08, DOC-13 to DOC-16).
    #[command(name = "check-docs")]
    Docs,
    /// Fails on a part of the committed `openapi.json` that breaks the rules for the HTTP
    /// contract: its paths, its property names, each operation's access level and response
    /// descriptions, and the codes of its error responses (API-01, API-05, API-17, ERR-02,
    /// ERR-13).
    #[command(name = "check-openapi")]
    Openapi,
    /// Fails on a log or span field name outside the list of telemetry names, a record name
    /// outside it, a message that isn't constant, an `#[instrument]` without `skip_all`, and an
    /// event at any level but `DEBUG`, or a span, in module code (OBS-01, OBS-03 to OBS-06).
    #[command(name = "check-instrument")]
    Instrument,
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
    };
    print(&outcome)
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
