//! The repository's custom CI checks, and the other tasks that `just` runs.
//! `just` runs each subcommand but `restore-plan`, which the end-to-end tests run (OPS-10), and
//! the API crates never depend on this one (ARCH-01).

mod acronyms;
mod alerts;
mod boards;
mod calendar;
mod cargo;
mod catalog;
mod contracts;
mod copy;
mod crate_deps;
mod declarations;
mod deploy;
mod deps;
mod docs;
mod env_drift;
mod formats;
mod inputs;
mod instrument;
mod layers;
mod layout;
mod load;
mod markdown;
mod metrics;
mod migrated;
mod migrations;
mod mirror;
mod new_module;
mod openapi;
mod ownership;
// part: removal
mod parts;
// end part: removal
mod pins;
mod promql;
mod query_cache;
mod references;
mod release;
mod rename;
mod repo;
mod restore;
mod routes;
mod rust;
mod schema;
mod server;
mod services;
mod shell;
mod sql;
mod sql_bans;
mod stack;
mod tasks;
mod telemetry_config;
mod tenancy;
mod test_map;
mod text;
mod unused;
mod words;

use std::{fmt, io, process::ExitCode};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "The repository's custom CI checks, and the other tasks that `just` runs")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Reports which spec behaviors tests prove, and fails on a test whose name doesn't start with
    /// a behavior or rule ID that exists, and on the rest that the doc of `test_map` names.
    #[command(name = "check-test-map")]
    TestMap,
    /// Fails on a route that a spec's Routes table lists a behavior for, when a test that sent a
    /// request is named for the behavior and none that sent to the route is, as the file that the
    /// tests wrote says and `test_map/sent.rs` details. `just test` runs it once the tests pass.
    #[command(name = "sent-routes")]
    SentRoutes {
        /// The file that `SENT_ROUTES` named for the tests.
        file: std::path::PathBuf,
    },
    /// Fails on a link, ID or named CI check that doesn't resolve, a history marker, an
    /// unfinished-work marker, a document outside the documentation set, an acronym not spelled out
    /// where a document first uses it, a sentence, paragraph or cell of more words than DOC-15
    /// allows, a spec, rule or tracker whose format breaks DOC-08 or DOC-14, or a spec's Routes
    /// table that doesn't match `openapi.json`.
    #[command(name = "check-docs")]
    Docs,
    /// Fails on a part of the committed `openapi.json` that breaks the rules for the HTTP contract,
    /// of those that docs/operations/ci.md says it reads.
    #[command(name = "check-openapi")]
    Openapi,
    /// Fails on a log or span field name outside the list of telemetry names, a record name outside
    /// it, a message that isn't constant, an `#[instrument]` without `skip_all`, an event at any
    /// level but `DEBUG`, or a span, in `app` code other than `main.rs`, and an exception to a
    /// print lint outside the places printing is allowed.
    #[command(name = "check-instrument")]
    Instrument,
    /// Fails on a metric declared or named outside OBS-11, a catalog that differs from the
    /// declarations, a dashboard or alert that reads a metric nothing declares or lists, a metric
    /// that nothing reads, a panel title that isn't a question, a dashboard that Grafana's users
    /// can change, or dashboards no overview links to all of.
    #[command(name = "check-metrics")]
    Metrics,
    /// Writes the metric catalog, `docs/operations/metrics.md`, from the declarations (OBS-12).
    #[command(name = "metric-catalog")]
    MetricCatalog,
    /// Fails on a tail sampling of the collector that doesn't keep the traces OBS-08 names, a
    /// configuration of the managed backend or of the proxy that doesn't keep that sampling, or a
    /// latency objective that differs between the sampling, the alerts and the latency dashboard.
    #[command(name = "check-telemetry-config")]
    TelemetryConfig,
    /// Fails on an alert without a severity, a runbook section, or a test that expects it to fire.
    #[command(name = "alert-rules")]
    AlertRules,
    /// Fails on an action, runner, image, download, crate install or tool version that isn't
    /// pinned, a toolchain other than the one `rust-toolchain.toml` pins, or a crate whose edition
    /// isn't the workspace's 2024.
    #[command(name = "check-pins")]
    Pins,
    /// Fails on a finding of shellcheck in a recipe of the justfile that runs on Linux and macOS,
    /// or in a shell script under `deploy/`, and on an `echo`, a function or a `psql` of such a
    /// script that `shell::scripts` refuses.
    #[command(name = "check-shell")]
    Shell,
    /// Fails on a function, method or macro that the locked sqlx exposes and that takes SQL text,
    /// outside the checked macros, and that `clippy.toml` doesn't ban, or on a ban of sqlx's that
    /// reaches nothing.
    #[command(name = "check-sql-bans")]
    SqlBans,
    /// Fails on `sqlx` in a module's routes, an HTTP type in its service, a routes type that
    /// deserializes without refusing unknown fields, an infrastructure error in a service or route
    /// signature, or an HTTP client built outside the places SEC-10 allows.
    #[command(name = "check-layers")]
    Layers,
    /// Fails on a module's public submodule, code that reaches into another module's submodule, a
    /// type of a module that a function its `mod.rs` re-exports names and `mod.rs` doesn't, a path
    /// of a template of `new-module` to what another module doesn't re-export, a use of a module
    /// the graph in architecture.md doesn't allow, a cycle in that graph, or another module's
    /// error in a module's public signatures, and on a dependency that its crate's code never
    /// names.
    #[command(name = "check-deps")]
    Deps,
    /// Fails on a table, column, constraint, function or index of the migrations' schema that
    /// breaks the data rules for names, standard columns, check constraints, functions, foreign
    /// keys and soft deletes, or a privilege, owner or setting of the roles that breaks them, as
    /// the catalog of a database that the API's own `bootstrap` and `migrate` build shows it.
    #[command(name = "check-schema")]
    Schema,
    /// Fails on a query, in a `sqlx` macro of `platform` or `app`, of a table with a
    /// `workspace_id` that names neither its `workspace_id` nor, for a table with one, its
    /// `user_id`, nor, for a table whose rows are addressed to a user, that address, or of a
    /// view that reads such a table that names no tenant column the view shows, outside a listed
    /// function of a job that spans tenants or of the reads that DATA-16 exempts, and on a table
    /// without a `workspace_id` that isn't listed as account or platform data or as the
    /// workspaces themselves, and on a migration's `CREATE` of a table or a view in a form that the
    /// check doesn't read.
    #[command(name = "check-tenancy")]
    Tenancy,
    /// Fails on a query, in a `sqlx` macro of `platform` or `app`, that names a table another
    /// module owns: the module whose name the table's creating migration holds. Fails on an
    /// insert of those queries that names its row's `id` too (DATA-08).
    #[command(name = "check-table-ownership")]
    TableOwnership,
    /// Fails on a source file or a deploy script over 500 lines, a module's file outside ARCH-03's
    /// set, a module without its error.rs, an exception to `dead_code`, and a job declared outside
    /// its module's jobs.rs or with a kind of another module's.
    #[command(name = "check-layout")]
    Layout,
    /// Fails on a migration whose name holds no module, on a migration that the newest release
    /// holds and the working tree changed or deleted, and on one that builds an index of another
    /// migration's table in its transaction (DATA-10).
    #[command(name = "check-migrations")]
    Migrations,
    /// Fails when the query cache in `.sqlx/` differs from what sqlx-cli writes from a database
    /// that the migrations build.
    #[command(name = "sqlx-check")]
    SqlxCheck,
    // part: removal
    /// Fails on a marker of an optional part that names no part, a line that holds two markers, a
    /// block that no end marker closes, a marker in a Rust file that isn't Rust's `//` comment
    /// alone on its line, an attribute or a doc comment above a Rust block that holds its item, a
    /// marker before more of its line outside a table row, a test of another module's behavior in
    /// a file that a part owns, outside a block of a part that marks the behavior's row, and, for
    /// each run that `crates/xtask/parts.yaml` lists and for each part alone, on a Rust file that
    /// removing the parts leaves unparseable, a line or path it keeps that names one of them, and a
    /// line it keeps that cites a decision, a behavior or a rule, or links to a file or a heading,
    /// that it deletes, or names a module's export that only lines it deletes give, removing them
    /// in memory.
    #[command(name = "check-parts")]
    Parts,
    /// Removes the optional parts named `parts` (ARCH-07): it deletes the files each owns whole
    /// and the lines its markers mark, outside Rust with a blank line where ARCH-07 says, writing
    /// "None." in each section of a spec that it empties,
    /// then the cached queries that no kept code holds, updates `Cargo.lock`, writes
    /// `openapi.json` and the metric catalog again, and formats the code, then fails while a file
    /// it kept, or the path of one, still names one of them.
    Remove {
        /// The parts, such as those a workspace shape doesn't keep (architecture §5).
        #[arg(required = true)]
        parts: Vec<String>,
    },
    /// Keeps the optional parts named `parts`, with which a fork chooses its parts once (ARCH-07):
    /// it removes every other part as `remove` does, `removal` among them, which holds what makes
    /// a part removable, then takes the markers out of every file.
    Keep {
        /// The parts that the product keeps, one that creates workspaces among them.
        #[arg(required = true)]
        parts: Vec<String>,
    },
    // end part: removal
    /// Writes a new module that holds one workspace resource, with its migration, permissions,
    /// spec and tests, registers it where every module is registered, rewrites the query cache
    /// from a database of its own and `openapi.json`, and formats the code (ARCH-03). A step that
    /// fails undoes what it wrote.
    #[command(name = "new-module")]
    NewModule {
        /// The module's name, plural and in snake_case, such as `order_items`.
        module: String,
        /// The name of one of its rows, singular, such as `order_item`; by default, the singular
        /// that English's regular plurals give the module's name, where only one kind of plural
        /// ends as it does.
        resource: Option<String>,
    },
    /// On a copy of the repository, writes two modules with `just new-module`, one after the
    /// other, then runs `just checks`.
    #[command(name = "check-new-module")]
    CheckNewModule,
    /// Renames the product: replaces its name and its slug with the same forms of `name` in every
    /// file that holds them, and formats the code (ARCH-15). It changes nothing while a file
    /// would still hold the old name in another form, or holds the new one already.
    Rename {
        /// The new name, as people read it, such as `Acme Notes`.
        name: String,
    },
    /// On a copy of the repository, renames the product with `just rename`, then runs `just
    /// checks`.
    #[command(name = "check-rename")]
    CheckRename,
    // part: removal
    /// On a copy of the repository for each run that `crates/xtask/parts.yaml` lists, or for the
    /// runs named, keeps every part that the run doesn't remove, or removes the run's parts where
    /// its task is `remove`, then runs `just checks`.
    #[command(name = "check-removable")]
    CheckRemovable {
        /// The runs, such as `personal`; every run by default.
        runs: Vec<String>,
    },
    // end part: removal
    /// Sets the app's version to `version` on `main`, commits it and pushes it, after it checks
    /// that `version` can be released from `main` as it is. The run on `main` that passes with
    /// the version releases it (OPS-12). Without a version, it shows the versions that can come
    /// next, and changes nothing but the `origin/main` and tags that it first fetches.
    Release {
        /// The version, such as 1.4.0, or 1.4.0-rc.1 for a prerelease; without one, the task
        /// shows the versions that can come next.
        version: Option<String>,
    },
    /// A task on a reference deployment's server (OPS-19).
    #[command(flatten)]
    Server(tasks::ServerTask),
    /// Prints the backup and the timeline of a restore to `moment`, or to the latest state, from
    /// what the backup bucket records, which deploy.sh's `restore-inputs` printed to standard
    /// input, as `just restore` plans one (OPS-15). The end-to-end tests run it, as they run
    /// deploy.sh without `just`.
    #[command(name = "restore-plan", hide = true)]
    RestorePlan {
        /// The moment: a time with its offset from UTC, as Postgres writes it; without one, the
        /// latest state.
        moment: Option<String>,
        /// The time at which the bucket recorded what the input holds, for a restore from the
        /// backups that it held then (OPS-22).
        #[arg(long)]
        bucket_as_of: Option<String>,
    },
    /// Warns of each setting of `.env.example` that `.env` lacks, or still sets to an earlier
    /// value of the example's, which `just setup` and `just dev` run (OPS-20).
    #[command(name = "env-drift")]
    EnvDrift,
    /// Writes the migrations that the newest release holds into a directory, which then holds
    /// nothing else. `just test` runs it before the tests, one of which applies the migrations
    /// on top of them (DATA-11).
    #[command(name = "release-migrations")]
    ReleaseMigrations {
        /// The directory that `RELEASE_MIGRATIONS` names for the tests.
        directory: std::path::PathBuf,
    },
    /// Restarts each running service of compose.yaml whose configuration, the files that it
    /// mounts from `deploy/`, changed since it started. `just services` runs it first.
    #[command(name = "stale-services")]
    StaleServices,
    /// Measures one replica's capacity: the highest constant rate of requests it serves with
    /// none failed and the 99th percentile within the latency objective (OPS-16).
    #[command(name = "load-test")]
    LoadTest {
        /// The route: `capabilities`, `GET /v1/capabilities`, which a release's notes give,
        /// `me`, `GET /v1/users/me` with a signed-in user's access token, `sign-ins`,
        /// `POST /v1/sessions` with a password, beside requests that hash nothing, or `streams`,
        /// which measures the replica's hub instead (OPS-16).
        #[arg(default_value = "capabilities")]
        route: String,
    },
    /// Fills the store of the mirror that Compose runs with the images the end-to-end tests'
    /// servers pull, for `platform`, such as linux/amd64 (TEST-06).
    Mirror {
        /// The servers' platform.
        platform: String,
    },
    /// Prints the platform and the images that `mirror` fills the mirror's store with for
    /// `platform`, as the store names what filled it (OPS-21).
    #[command(name = "mirror-images")]
    MirrorImages {
        /// The servers' platform.
        platform: String,
    },
    /// Prints an identifier of the files under `directory` as they stand in the working tree,
    /// or, without one, of the files that the API's image is built from, which an image built
    /// from them carries (OPS-14).
    Inputs {
        /// The directory, as a path from the repository root.
        directory: Option<String>,
    },
    /// Fails unless the API's image in target/image is built from the files of the working
    /// tree, as its label says. `just e2e` and `just load-test` run it first (OPS-20).
    #[command(name = "image-inputs")]
    ImageInputs,
    /// Fails unless `version` can be a new release after the repository's release tags. CI runs
    /// it before it releases a version (OPS-12).
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
        Command::Server(task) => return task.run(),
        // part: removal
        Command::Remove { parts } => return parts::remove(&parts),
        Command::Keep { parts } => return parts::keep(&parts),
        Command::CheckRemovable { runs } => return parts::check_removable(&runs),
        // end part: removal
        Command::NewModule { module, resource } => return new_module::run(&module, resource),
        Command::CheckNewModule => return new_module::check(),
        Command::Rename { name } => return rename::run(&name),
        Command::CheckRename => return rename::check(),
        Command::Release { version } => return release::run(version.as_deref()),
        Command::ReleaseCheck { version } => return release::check(&version),
        Command::Inputs { directory } => return inputs::run(directory.as_deref()),
        Command::ImageInputs => return inputs::check_image(),
        Command::LoadTest { route } => return load::run(&route),
        Command::EnvDrift => return env_drift::run(),
        Command::StaleServices => return services::run(),
        Command::Mirror { platform } => return mirror::run(&platform),
        Command::MirrorImages { platform } => return mirror::list(&platform),
        Command::RestorePlan {
            moment,
            bucket_as_of,
        } => return restore::run(moment.as_deref(), bucket_as_of.as_deref()),
        command => command,
    };
    let repo = match repo::Repo::load() {
        Ok(repo) => repo,
        Err(error) => return unreadable(&error),
    };
    let catalog = catalog::Catalog::read(&repo);
    let outcome = match command {
        Command::TestMap => test_map::check(&repo, &catalog),
        Command::SentRoutes { file } => test_map::sent::check_file(&repo, &file),
        Command::Docs => docs::check(&repo, &catalog),
        Command::Openapi => openapi::check_against_release(&repo),
        Command::Instrument => instrument::check(&repo),
        Command::Metrics => metrics::check(&repo),
        Command::MetricCatalog => write_catalog(&repo),
        Command::TelemetryConfig => telemetry_config::check(&repo),
        Command::AlertRules => alerts::check(&repo),
        Command::Pins => pins::check(&repo),
        Command::Shell => shell::check(&repo),
        Command::SqlBans => sql_bans::check(&repo),
        Command::Layers => layers::check(&repo),
        Command::Deps => {
            let mut outcome = deps::check(&repo);
            outcome.findings.extend(crate_deps::check(&repo));
            outcome
        }
        Command::Schema => schema::check(&repo),
        Command::Tenancy => tenancy::check(&repo),
        Command::TableOwnership => ownership::check(&repo),
        Command::Layout => layout::check(&repo),
        Command::Migrations => migrations::check(&repo),
        Command::ReleaseMigrations { directory } => migrations::write_release(&directory),
        Command::SqlxCheck => query_cache::check(),
        // part: removal
        Command::Parts => parts::check(&repo),
        Command::Remove { .. } | Command::Keep { .. } | Command::CheckRemovable { .. } => {
            Outcome::default()
        }
        // end part: removal
        Command::NewModule { .. }
        | Command::CheckNewModule
        | Command::Rename { .. }
        | Command::CheckRename
        | Command::Release { .. }
        | Command::ReleaseCheck { .. }
        | Command::Inputs { .. }
        | Command::ImageInputs
        | Command::Server(_)
        | Command::RestorePlan { .. }
        | Command::LoadTest { .. }
        | Command::EnvDrift
        | Command::StaleServices
        | Command::Mirror { .. }
        | Command::MirrorImages { .. } => Outcome::default(),
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
    use crate::tasks::ServerTask;

    /// The version and the option of a deploy to `production` with `args`, if they make one.
    fn deploy(args: &[&str]) -> Option<(Option<String>, bool)> {
        let cli = Cli::try_parse_from([&["xtask", "deploy", "production"], args].concat()).ok()?;
        match cli.command {
            Command::Server(ServerTask::Deploy {
                version,
                allow_restart,
                ..
            }) => Some((version, allow_restart)),
            _ => None,
        }
    }

    #[test]
    fn ops_14_a_deploy_s_version_is_optional_and_its_option_stays_an_option() {
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
            Command::Server(ServerTask::Restore {
                moment,
                allow_unarchived,
                ..
            }) => Some((moment, allow_unarchived)),
            _ => None,
        }
    }

    #[test]
    fn ops_15_a_restore_s_moment_is_optional_and_its_option_stays_an_option() {
        let moment = "2026-10-25 10:41:31+00";
        assert_eq!(restore(&[""]), Some((Some(String::new()), false)));
        assert_eq!(restore(&["--allow-unarchived"]), Some((None, true)));
        assert_eq!(restore(&[moment]), Some((Some(moment.to_owned()), false)));
        assert_eq!(
            restore(&[moment, "--allow-unarchived"]),
            Some((Some(moment.to_owned()), true))
        );
    }

    #[test]
    fn ops_22_a_restore_takes_the_bucket_s_time_in_one_word_or_as_its_date_and_its_time() {
        // The time of a restore on `production` with `args`, which the justfile passes as the
        // words of its options, so a time that holds a space arrives as two.
        let held = |args: &[&str]| {
            let cli = Cli::try_parse_from([&["xtask", "restore", "production"], args].concat());
            match cli.map(|cli| cli.command) {
                Ok(Command::Server(ServerTask::Restore { bucket_as_of, .. })) => {
                    Some(bucket_as_of.join(" "))
                }
                _ => None,
            }
        };

        assert_eq!(held(&[""]), Some(String::new()));
        assert_eq!(
            held(&["", "--bucket-as-of", "2026-10-25T10:41:31+00"]),
            Some("2026-10-25T10:41:31+00".to_owned())
        );
        assert_eq!(
            held(&["", "--bucket-as-of", "2026-10-25", "10:41:31+00"]),
            Some("2026-10-25 10:41:31+00".to_owned())
        );
        assert_eq!(
            held(&[
                "2026-10-25 10:00:00+00",
                "--bucket-as-of=2026-10-25T10:41:31Z"
            ]),
            Some("2026-10-25T10:41:31Z".to_owned())
        );
        // It checks nothing of the database it replaces, so the option that lets a restore go on
        // past that check is refused beside it; and it takes a time.
        assert_eq!(
            held(&[
                "",
                "--bucket-as-of",
                "2026-10-25T10:41:31Z",
                "--allow-unarchived"
            ]),
            None
        );
        assert_eq!(held(&["", "--bucket-as-of"]), None);
    }

    #[test]
    fn ops_19_a_key_s_replacement_takes_a_server_and_the_key_s_file() {
        let cli = Cli::try_parse_from(["xtask", "replace-key", "production", "hmac_key"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Server(ServerTask::ReplaceKey { host, key })
                if host == "production" && key == "hmac_key"
        ));
        assert!(Cli::try_parse_from(["xtask", "replace-key", "production"]).is_err());
    }

    #[test]
    fn ops_19_the_removal_of_a_second_factor_takes_a_server_and_an_address() {
        let cli = Cli::try_parse_from([
            "xtask",
            "remove-second-factor",
            "production",
            "ada@example.com",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Command::Server(ServerTask::RemoveSecondFactor { host, email })
                if host == "production" && email == "ada@example.com"
        ));
        assert!(Cli::try_parse_from(["xtask", "remove-second-factor", "production"]).is_err());
    }

    #[test]
    fn ops_19_the_end_of_sessions_takes_a_server_and_an_address_or_all() {
        let cli = |arguments: &[&str]| {
            Cli::try_parse_from(
                ["xtask", "end-sessions"]
                    .into_iter()
                    .chain(arguments.iter().copied()),
            )
        };
        let one = cli(&["production", "ada@example.com"]).unwrap();
        let all = cli(&["production", "--all"]).unwrap();

        assert!(matches!(
            one.command,
            Command::Server(ServerTask::EndSessions { host, whose })
                if host == "production" && whose == "ada@example.com"
        ));
        assert!(matches!(
            all.command,
            Command::Server(ServerTask::EndSessions { host, whose })
                if host == "production" && whose == "--all"
        ));
        assert!(cli(&["production"]).is_err());
    }

    // part: files
    #[test]
    fn ops_19_the_writing_back_of_files_takes_a_server_and_a_moment() {
        let cli = Cli::try_parse_from([
            "xtask",
            "restore-files",
            "production",
            "2026-10-25 10:30:00+00",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Command::Server(ServerTask::RestoreFiles { host, moment })
                if host == "production" && moment == "2026-10-25 10:30:00+00"
        ));
        assert!(Cli::try_parse_from(["xtask", "restore-files", "production"]).is_err());
    }
    // end part: files

    // part: workspace_setup
    #[test]
    fn ops_19_the_setup_of_a_workspace_takes_a_server_a_name_an_address_and_maybe_a_name() {
        let cli = |arguments: &[&str]| {
            Cli::try_parse_from(
                ["xtask", "setup-workspace"]
                    .into_iter()
                    .chain(arguments.iter().copied()),
            )
        };
        let named = cli(&["production", "Acme Labs", "ada@example.com", "Ada Lovelace"]).unwrap();
        let unnamed = cli(&["production", "Acme Labs", "ada@example.com"]).unwrap();

        assert!(matches!(
            named.command,
            Command::Server(ServerTask::SetupWorkspace { host, name, email, owner_name })
                if host == "production"
                    && name == "Acme Labs"
                    && email == "ada@example.com"
                    && owner_name.as_deref() == Some("Ada Lovelace")
        ));
        assert!(matches!(
            unnamed.command,
            Command::Server(ServerTask::SetupWorkspace {
                owner_name: None,
                ..
            })
        ));
        assert!(cli(&["production", "Acme Labs"]).is_err());
    }
    // end part: workspace_setup

    // part: superadmins
    #[test]
    fn ops_19_the_superadmins_task_takes_a_server_and_what_to_do_there() {
        let cli = |arguments: &[&str]| {
            Cli::try_parse_from(
                ["xtask", "superadmin"]
                    .into_iter()
                    .chain(arguments.iter().copied()),
            )
        };
        let added = cli(&["production", "add", "ada@example.com"]).unwrap();
        let removed = cli(&["production", "remove", "ada@example.com"]).unwrap();
        let listed = cli(&["production", "list"]).unwrap();

        assert!(matches!(
            added.command,
            Command::Server(ServerTask::Superadmin {
                host,
                command: crate::tasks::SuperadminTask::Add { email },
            }) if host == "production" && email == "ada@example.com"
        ));
        assert!(matches!(
            removed.command,
            Command::Server(ServerTask::Superadmin {
                command: crate::tasks::SuperadminTask::Remove { email },
                ..
            }) if email == "ada@example.com"
        ));
        assert!(matches!(
            listed.command,
            Command::Server(ServerTask::Superadmin {
                command: crate::tasks::SuperadminTask::List,
                ..
            })
        ));
        // An address names an account to change, and a list takes none.
        assert!(cli(&["production", "list", "ada@example.com"]).is_err());
        assert!(cli(&["production", "add"]).is_err());
        assert!(cli(&["production"]).is_err());
    }
    // end part: superadmins
}
