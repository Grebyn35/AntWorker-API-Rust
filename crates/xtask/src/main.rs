//! The repository's custom CI checks, and the tasks that `just` runs to release, deploy and test.
//! Each subcommand runs as a `just` task (OPS-12), and the API crates never depend on this one
//! (ARCH-01).

mod acronyms;
mod alerts;
mod boards;
mod catalog;
mod declarations;
mod deploy;
mod docs;
mod formats;
mod inputs;
mod instrument;
mod load;
mod markdown;
mod metrics;
mod mirror;
mod openapi;
mod pins;
mod promql;
mod release;
mod repo;
mod restore;
mod routes;
mod server;
mod shell;
mod stack;
mod telemetry_config;
mod test_map;
mod text;

use std::{fmt, io, process::ExitCode};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "The repository's custom CI checks, and the tasks that release, deploy and test")]
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
    /// metric that nothing reads, a panel title that isn't a question, a dashboard that Grafana's
    /// users can change, or dashboards no overview links to all of (OBS-11 to OBS-13).
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
    /// Fails on an action, runner, image, download, crate install or tool version that isn't
    /// pinned, a toolchain other than the one `rust-toolchain.toml` pins, or a crate whose
    /// edition isn't the workspace's 2024 (CODE-01, OPS-13).
    #[command(name = "check-pins")]
    Pins,
    /// Fails on a finding of shellcheck in a recipe of the justfile that runs on Linux and
    /// macOS, or in a shell script under `deploy/` (CODE-16).
    #[command(name = "check-shell")]
    Shell,
    /// Sets the app's version to `version` on `main`, commits it and pushes it, after it checks
    /// that `version` can be released from `main` as it is. The run on `main` that passes with
    /// the version releases it (OPS-14). Without a version, it shows the versions that can come
    /// next, and changes nothing.
    Release {
        /// The version, such as 1.4.0, or 1.4.0-rc.1 for a prerelease; without one, the task
        /// shows the versions that can come next.
        version: Option<String>,
    },
    /// Deploys the release `version`, or the latest release, to the reference deployment that SSH
    /// reaches at `host` (OPS-16).
    Deploy {
        /// The server, as SSH names it: user@address, or a Host of ~/.ssh/config.
        host: String,
        /// The release's version, such as 1.4.0; the latest by default.
        version: Option<String>,
        /// Restarts Postgres or Caddy if the release changes them, which interrupts requests for
        /// a few seconds.
        #[arg(long)]
        allow_restart: bool,
    },
    /// Takes a full base backup of the bundled database of the deployment at `host` (OPS-17).
    Backup {
        /// The server, as SSH names it.
        host: String,
    },
    /// Restores the bundled database of the deployment at `host`, as it stood at `moment`, or at
    /// the latest moment its backups hold (OPS-17).
    Restore {
        /// The server, as SSH names it.
        host: String,
        /// The moment: a time with its offset from UTC, as Postgres writes it.
        moment: Option<String>,
        /// Restores although the database's log doesn't reach the bucket, which leaves the
        /// changes that the bucket lacks only in the data that the restore moves aside.
        #[arg(long)]
        allow_unarchived: bool,
    },
    /// Prints the backup and the timeline of a restore to `moment`, or to the latest state, from
    /// what the backup bucket records, which deploy.sh's `restore-inputs` printed to standard
    /// input, as `just restore` plans one (OPS-17). The end-to-end tests run it, as they run
    /// deploy.sh without `just`.
    #[command(name = "restore-plan", hide = true)]
    RestorePlan {
        /// The moment: a time with its offset from UTC, as Postgres writes it; without one, the
        /// latest state.
        moment: Option<String>,
    },
    /// Shows the state of the deployment at `host`: which color serves, its containers, its
    /// backups, and the server's free disk and memory (OPS-22).
    Status {
        /// The server, as SSH names it.
        host: String,
    },
    /// Sets up the server that SSH reaches at `host`, as root or a user that may run sudo without
    /// a password: installs Docker, prepares the deployment's user, and writes the settings and
    /// the secrets it asks for (OPS-22). Run again, it changes them.
    #[command(name = "server-setup")]
    ServerSetup {
        /// The server, as SSH names it.
        host: String,
        /// The release whose files the setup follows, such as 1.4.0; the latest by default.
        version: Option<String>,
    },
    /// Checks what a deploy of the release `version`, or of the latest release, to the
    /// deployment at `host` needs, and changes nothing (OPS-22).
    #[command(name = "server-check")]
    ServerCheck {
        /// The server, as SSH names it, as the user that runs the deployment.
        host: String,
        /// The release's version, such as 1.4.0.
        version: Option<String>,
    },
    /// Shows the last lines of the logs of the deployment's services at `host` (OPS-22).
    Logs {
        /// The server, as SSH names it.
        host: String,
        /// The services, `api` for both colors' replicas, or none for all of them, and
        /// `--follow` to go on showing the lines that follow.
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        arguments: Vec<String>,
    },
    /// Measures one replica's capacity: the highest constant rate of requests it serves with
    /// none failed and the 99th percentile within the latency objective (OPS-18).
    #[command(name = "load-test")]
    LoadTest {
        /// The route: `capabilities`, `GET /v1/capabilities`, which a release's notes give, or
        /// `me`, `GET /v1/users/me` with a signed-in user's access token.
        #[arg(default_value = "capabilities")]
        route: String,
    },
    /// Fills the cache of Docker Hub that Compose runs with the images the end-to-end tests'
    /// servers pull, for `platform`, such as linux/amd64 (TEST-07).
    Mirror {
        /// The servers' platform.
        platform: String,
    },
    /// Prints an identifier of the files under `directory` as they stand in the working tree,
    /// which an image built from them carries (OPS-16).
    Inputs {
        /// The directory, as a path from the repository root.
        directory: String,
    },
    /// Fails unless `version` can be a new release after the repository's release tags. CI runs
    /// it before it releases a version (OPS-14).
    #[command(name = "release-check")]
    ReleaseCheck {
        /// The version that `crates/app/Cargo.toml` sets.
        version: String,
    },
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
    let command = match Cli::parse().command {
        Command::Release { version } => return release::run(version.as_deref()),
        Command::ReleaseCheck { version } => return release::check(&version),
        Command::Inputs { directory } => return inputs::run(&directory),
        Command::Deploy {
            host,
            version,
            allow_restart,
        } => return deploy::deploy(&host, version.as_deref(), allow_restart),
        Command::Backup { host } => return deploy::backup(&host),
        Command::LoadTest { route } => return load::run(&route),
        Command::Mirror { platform } => return mirror::run(&platform),
        Command::Restore {
            host,
            moment,
            allow_unarchived,
        } => return deploy::restore(&host, moment.as_deref(), allow_unarchived),
        Command::RestorePlan { moment } => return restore::run(moment.as_deref()),
        Command::Status { host } => return deploy::status(&host),
        Command::Logs { host, arguments } => return deploy::logs(&host, &arguments),
        Command::ServerSetup { host, version } => return server::setup(&host, version.as_deref()),
        Command::ServerCheck { host, version } => return server::check(&host, version.as_deref()),
        command => command,
    };
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
        Command::Pins => pins::check(&repo),
        Command::Shell => shell::check(&repo),
        Command::Release { .. }
        | Command::ReleaseCheck { .. }
        | Command::Inputs { .. }
        | Command::Deploy { .. }
        | Command::Backup { .. }
        | Command::Restore { .. }
        | Command::RestorePlan { .. }
        | Command::Status { .. }
        | Command::Logs { .. }
        | Command::ServerSetup { .. }
        | Command::ServerCheck { .. }
        | Command::LoadTest { .. }
        | Command::Mirror { .. } => Outcome::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The version and the option of a deploy to `production` with `args`, if they make one.
    fn deploy(args: &[&str]) -> Option<(Option<String>, bool)> {
        let cli = Cli::try_parse_from([&["xtask", "deploy", "production"], args].concat()).ok()?;
        match cli.command {
            Command::Deploy {
                version,
                allow_restart,
                ..
            } => Some((version, allow_restart)),
            _ => None,
        }
    }

    #[test]
    fn ops_16_a_deploy_s_version_is_optional_and_its_option_stays_an_option() {
        assert_eq!(deploy(&[]), Some((None, false)));
        assert_eq!(deploy(&["--allow-restart"]), Some((None, true)));
        assert_eq!(deploy(&["1.4.0"]), Some((Some("1.4.0".to_owned()), false)));
        assert_eq!(
            deploy(&["1.4.0", "--allow-restart"]),
            Some((Some("1.4.0".to_owned()), true))
        );
    }

    /// The moment and the option of a restore on `production` with `args`, as the justfile
    /// passes them: the moment, empty without one, then the options.
    fn restore(args: &[&str]) -> Option<(Option<String>, bool)> {
        let cli = Cli::try_parse_from([&["xtask", "restore", "production"], args].concat()).ok()?;
        match cli.command {
            Command::Restore {
                moment,
                allow_unarchived,
                ..
            } => Some((moment, allow_unarchived)),
            _ => None,
        }
    }

    #[test]
    fn ops_17_a_restore_s_moment_is_optional_and_its_option_stays_an_option() {
        let moment = "2026-10-25 10:41:31+00";
        assert_eq!(restore(&[""]), Some((Some(String::new()), false)));
        assert_eq!(restore(&["--allow-unarchived"]), Some((None, true)));
        assert_eq!(restore(&[moment]), Some((Some(moment.to_owned()), false)));
        assert_eq!(
            restore(&[moment, "--allow-unarchived"]),
            Some((Some(moment.to_owned()), true))
        );
    }
}
