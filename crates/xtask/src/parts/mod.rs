//! Removing an optional part, `just remove` (ARCH-07), and `check-removable`, which runs it on
//! copies of the repository, one for each run that `parts.yaml` lists, then `just checks` on each.

mod cache;
mod copy;
mod markers;

pub use markers::unmarked;

use std::{path::Path, process::ExitCode};

use serde_json::Value;

use crate::{
    Finding, metrics,
    repo::{self, Repo},
};

/// The list of the optional parts and of `check-removable`'s runs, from the repository root.
const LIST: &str = "crates/xtask/parts.yaml";

/// The migration workspace (DOC-04), which the migration's close deletes, so a removal neither
/// edits it nor reads it (ARCH-07).
const MIGRATION: [&str; 2] = ["docs/inventory/", "docs/MIGRATION.md"];

/// An optional part, as `parts.yaml` lists it.
struct Part {
    name: String,
    module: Option<String>,
    paths: Vec<String>,
    words: Vec<String>,
}

/// A run of `check-removable`: the parts it removes from a copy of its own.
struct Run {
    name: String,
    parts: Vec<String>,
}

/// The parts and the runs that `parts.yaml` lists.
fn list(repo: &Repo) -> Result<(Vec<Part>, Vec<Run>), String> {
    let file = repo
        .file(LIST)
        .ok_or_else(|| format!("{LIST} is missing"))?;
    let value: Value =
        serde_saphyr::from_str(&file.text).map_err(|error| format!("{LIST}: {error}"))?;
    let strings = |value: &Value, key: &str| -> Vec<String> {
        value[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let entries = |key: &str| value[key].as_array().cloned().unwrap_or_default();
    let parts = entries("parts")
        .iter()
        .map(|part| Part {
            name: part["name"].as_str().unwrap_or_default().to_owned(),
            module: part["module"].as_str().map(str::to_owned),
            paths: strings(part, "paths"),
            words: strings(part, "words"),
        })
        .collect();
    let runs = entries("runs")
        .iter()
        .map(|run| Run {
            name: run["name"].as_str().unwrap_or_default().to_owned(),
            parts: strings(run, "parts"),
        })
        .collect();
    Ok((parts, runs))
}

/// Whether `path` is one of the migration's own documents.
fn is_migration(path: &str) -> bool {
    MIGRATION.iter().any(|kept| path.starts_with(kept))
}

/// Fails on each marker of the repository's files that names no part, each block that no end
/// marker closes, and each test, in a file that a part owns whole, that another module's behavior
/// names, since the removal would take it (ARCH-07).
fn validate(repo: &Repo, parts: &[Part]) -> Vec<Finding> {
    let known: Vec<&str> = parts.iter().map(|part| part.name.as_str()).collect();
    let mut findings: Vec<Finding> = repo
        .files()
        .filter(|file| !is_migration(&file.path))
        .filter_map(|file| {
            markers::owners(&file.text, &known)
                .err()
                .map(|(line, message)| Finding::new(&file.path, line, message))
        })
        .collect();
    for part in parts {
        let Some(module) = &part.module else {
            continue;
        };
        let owned = owned(repo, part);
        for file in repo
            .files()
            .filter(|file| file.path.ends_with(".rs") && owned.contains(&file.path))
        {
            for (index, line) in file.text.lines().enumerate() {
                let Some(name) = line
                    .trim_start()
                    .strip_prefix("async fn ")
                    .or_else(|| line.trim_start().strip_prefix("fn "))
                else {
                    continue;
                };
                if let Some(other) = behavior_module(name).filter(|other| other != module) {
                    findings.push(Finding::new(
                        &file.path,
                        index + 1,
                        format!(
                            "a test of `{other}`'s behavior is in a file that `{}` owns, so \
                             removing the part would remove it: move it, or prove the behavior \
                             without the part (TEST-05)",
                            part.name
                        ),
                    ));
                }
            }
        }
    }
    findings
}

/// The module whose behavior the function `name` is named after, as `workspaces` for
/// `workspaces_005_...`: the words before its three-digit number.
fn behavior_module(name: &str) -> Option<String> {
    let name: String = name
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let words: Vec<&str> = name.split('_').collect();
    let number = words
        .iter()
        .position(|word| word.len() == 3 && word.chars().all(|c| c.is_ascii_digit()))?;
    (number > 0 && number + 1 < words.len()).then(|| words[..number].join("_"))
}

/// The files that `part` owns whole, text or not: its paths, and its module's migrations.
fn owned(repo: &Repo, part: &Part) -> Vec<String> {
    let under = |path: &str, owner: &str| {
        path == owner || path.starts_with(&format!("{}/", owner.trim_end_matches('/')))
    };
    let mut owned: Vec<String> = repo
        .paths()
        .filter(|path| part.paths.iter().any(|owner| under(path, owner)))
        .map(str::to_owned)
        .collect();
    if let Some(module) = &part.module {
        owned.extend(
            repo.paths()
                .filter(|path| {
                    path.strip_prefix("migrations/")
                        .and_then(|name| name.split_once('_'))
                        .is_some_and(|(stamp, rest)| {
                            stamp.chars().all(|c| c.is_ascii_digit())
                                && rest.starts_with(&format!("{module}_"))
                                && rest.ends_with(".sql")
                        })
                })
                .map(str::to_owned),
        );
    }
    owned
}

/// Each line of a file that the removal kept, outside the migration's documents, that holds a
/// word of a removed part, and each kept path that does, text or not.
fn leftovers(repo: &Repo, removed: &[&Part]) -> Vec<Finding> {
    let words: Vec<(&str, String)> = removed
        .iter()
        .flat_map(|part| {
            part.words
                .iter()
                .map(|word| (part.name.as_str(), word.to_ascii_lowercase()))
        })
        .collect();
    let mut findings = Vec::new();
    for path in repo.paths().filter(|path| !is_migration(path)) {
        for (part, word) in &words {
            if path.to_ascii_lowercase().contains(word) {
                findings.push(Finding::new(
                    path,
                    1,
                    format!("its path names `{part}`, which was removed (ARCH-07)"),
                ));
            }
        }
    }
    for file in repo.files().filter(|file| !is_migration(&file.path)) {
        for (index, line) in file.text.lines().enumerate() {
            let line = line.to_ascii_lowercase();
            for (part, word) in &words {
                if line.contains(word) {
                    findings.push(Finding::new(
                        &file.path,
                        index + 1,
                        format!("names `{part}`, which was removed (ARCH-07)"),
                    ));
                }
            }
        }
    }
    findings
}

/// Removes the parts named `names` from the repository (ARCH-07): it deletes the files they own
/// whole and the lines their markers mark, then the cached queries that no kept code holds,
/// updates `Cargo.lock`, writes `openapi.json` and the metric catalog again, formats the code, and
/// fails while a file it kept still names one of them.
pub fn remove(names: &[String]) -> ExitCode {
    let repo = match Repo::load() {
        Ok(repo) => repo,
        Err(error) => return fail(&format!("can't read the repository: {error}")),
    };
    let (parts, _) = match list(&repo) {
        Ok(list) => list,
        Err(error) => return fail(&error),
    };
    let known: Vec<&str> = parts.iter().map(|part| part.name.as_str()).collect();
    if let Some(unknown) = names.iter().find(|name| !known.contains(&name.as_str())) {
        return fail(&format!(
            "no part is named `{unknown}`; the parts are: {}",
            known.join(", ")
        ));
    }
    let findings = validate(&repo, &parts);
    if !findings.is_empty() {
        return failures(
            "the parts' markers or files are wrong, so nothing was removed",
            &findings,
        );
    }
    let removed: Vec<&Part> = parts
        .iter()
        .filter(|part| names.contains(&part.name))
        .collect();
    let removing: Vec<&str> = removed.iter().map(|part| part.name.as_str()).collect();
    let (deleted, edited) = match delete(&repo, &removed, &known) {
        Ok(counts) => counts,
        Err(error) => return fail(&error),
    };
    let stale = match prune_cache() {
        Ok(stale) => stale,
        Err(error) => return fail(&error),
    };
    report(&format!(
        "removed {}: deleted {deleted} files and {stale} entries of the query cache, and edited \
         {edited}",
        removing.join(", "),
    ));
    if let Err(error) = regenerate() {
        return fail(&error);
    }
    match Repo::load() {
        Ok(repo) => {
            let findings = leftovers(&repo, &removed);
            if findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                failures(
                    "these lines still name what was removed: give each its part's marker, or \
                     rewrite it not to name the part (ARCH-07)",
                    &findings,
                )
            }
        }
        Err(error) => fail(&format!("can't read the repository: {error}")),
    }
}

/// Deletes the files that the parts `removed` own whole, and the lines their markers mark in every
/// other file, but the migration's documents; returns how many files it deleted and edited.
fn delete(repo: &Repo, removed: &[&Part], known: &[&str]) -> Result<(usize, usize), String> {
    let removing: Vec<&str> = removed.iter().map(|part| part.name.as_str()).collect();
    let deleted: Vec<String> = removed.iter().flat_map(|part| owned(repo, part)).collect();
    for path in &deleted {
        repo::delete(path).map_err(|error| format!("can't delete {path}: {error}"))?;
    }
    let mut edited = 0;
    for file in repo
        .files()
        .filter(|file| !is_migration(&file.path) && !deleted.contains(&file.path))
    {
        let stripped = markers::strip(&file.text, &removing, known)
            .map_err(|(line, message)| format!("{}:{line}: {message}", file.path))?;
        if let Some(text) = stripped {
            repo::write(&file.path, &text)
                .map_err(|error| format!("can't write {}: {error}", file.path))?;
            edited += 1;
        }
    }
    Ok((deleted.len(), edited))
}

/// Deletes the entries of the query cache whose query no kept code holds (DATA-20), and returns
/// how many it deleted.
fn prune_cache() -> Result<usize, String> {
    let repo = Repo::load().map_err(|error| format!("can't read the repository: {error}"))?;
    let stale = cache::stale(&repo).map_err(|finding| finding.to_string())?;
    for path in &stale {
        repo::delete(path).map_err(|error| format!("can't delete {path}: {error}"))?;
    }
    Ok(stale.len())
}

/// Updates `Cargo.lock` to the dependencies left, offline, so it only drops packages, writes
/// `openapi.json` and the metric catalog again, from the code that the removal kept, and formats
/// the code.
fn regenerate() -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    copy::run(&root, "cargo", &["update", "--workspace", "--offline"], &[])?;
    copy::run(
        &root,
        "cargo",
        &[
            "run",
            "--locked",
            "--quiet",
            "--package",
            "app",
            "--bin",
            "antworker-api",
            "--",
            "openapi",
            "openapi.json",
        ],
        &[("SQLX_OFFLINE", "true")],
    )?;
    let repo = Repo::load().map_err(|error| error.to_string())?;
    let catalog = metrics::write(&repo).map_err(|findings| {
        findings
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    })?;
    repo::write(metrics::CATALOG, &catalog).map_err(|error| error.to_string())?;
    copy::run(&root, "cargo", &["fmt", "--all"], &[])
}

/// Runs `check-removable` (ARCH-07): for each of the runs named `names`, or every run, on a copy
/// of the repository of its own, `just remove` with the run's parts, then `just checks`.
pub fn check_removable(names: &[String]) -> ExitCode {
    let repo = match Repo::load() {
        Ok(repo) => repo,
        Err(error) => return fail(&format!("can't read the repository: {error}")),
    };
    let (parts, runs) = match list(&repo) {
        Ok(list) => list,
        Err(error) => return fail(&error),
    };
    let findings = validate(&repo, &parts);
    if !findings.is_empty() {
        return failures("the parts' markers or files are wrong", &findings);
    }
    if let Some(unknown) = names
        .iter()
        .find(|name| !runs.iter().any(|run| &run.name == *name))
    {
        return fail(&format!("no run is named `{unknown}`"));
    }
    let mut failed = Vec::new();
    for run in runs
        .iter()
        .filter(|run| names.is_empty() || names.contains(&run.name))
    {
        if run.parts.is_empty() {
            report(&format!(
                "{}: every part it removes is removed already",
                run.name
            ));
            continue;
        }
        report(&format!("{}: removing {}", run.name, run.parts.join(", ")));
        match copy::check(&run.name, &run.parts) {
            Ok(()) => report(&format!("{}: passed", run.name)),
            Err(error) => {
                report(&format!("{}: failed: {error}", run.name));
                failed.push(run.name.clone());
            }
        }
    }
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        fail(&format!("these runs failed: {}", failed.join(", ")))
    }
}

#[expect(clippy::print_stdout, reason = "the report is the task's output")]
fn report(message: &str) {
    println!("{message}");
}

#[expect(clippy::print_stderr, reason = "the error is the task's output")]
fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[expect(clippy::print_stderr, reason = "the findings are the task's output")]
fn failures(message: &str, findings: &[Finding]) -> ExitCode {
    for finding in findings {
        eprintln!("{finding}");
    }
    fail(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offers() -> Part {
        Part {
            name: "offers".to_owned(),
            module: Some("offers".to_owned()),
            paths: vec![
                "crates/app/src/modules/offers".to_owned(),
                "crates/app/tests/integration/offers.rs".to_owned(),
                "docs/spec/offers.md".to_owned(),
            ],
            words: vec!["offer".to_owned()],
        }
    }

    #[test]
    fn arch_07_a_module_owns_its_paths_and_its_migrations() {
        let repo = Repo::of(&[
            ("crates/app/src/modules/offers/mod.rs", "mod repo;"),
            (
                "migrations/20260101000000_offers_offers.sql",
                "CREATE TABLE offers ();",
            ),
            (
                "migrations/20260101000100_offers_more.sql",
                "ALTER TABLE offers ADD x int;",
            ),
            (
                "migrations/20260101000200_platform_jobs.sql",
                "CREATE TABLE jobs ();",
            ),
            ("docs/spec/offers.md", "# Spec"),
        ]);

        let mut owned = owned(&repo, &offers());
        owned.sort();

        assert_eq!(
            owned,
            [
                "crates/app/src/modules/offers/mod.rs",
                "docs/spec/offers.md",
                "migrations/20260101000000_offers_offers.sql",
                "migrations/20260101000100_offers_more.sql",
            ]
        );
    }

    #[test]
    fn arch_07_a_test_of_another_module_s_behavior_in_a_part_s_file_fails() {
        let tests = "#[tokio::test]\nasync fn offers_001_an_offer_is_made() {}\n\
                     #[test]\nfn arch_07_a_rule() {}\n\
                     #[tokio::test]\nasync fn platform_012_a_job_runs_for_its_scope() {}\n";
        let repo = Repo::of(&[
            (LIST, "parts:\n"),
            ("crates/app/tests/integration/offers.rs", tests),
        ]);

        let findings = validate(&repo, &[offers()]);

        let places: Vec<(&str, usize)> = findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line))
            .collect();
        assert_eq!(places, [("crates/app/tests/integration/offers.rs", 6)]);
        assert_eq!(
            behavior_module("client_telemetry_002_x()"),
            Some("client_telemetry".to_owned())
        );
        assert_eq!(behavior_module("ops_17_a_restore()"), None);
    }

    #[test]
    fn arch_07_a_removal_fails_on_each_line_that_still_names_the_part() {
        let repo = Repo::of(&[
            ("src/a.rs", "fn a() {}\n// The Offer's price.\n"),
            ("docs/inventory/old.md", "The offers of the old API."),
            ("src/offers_test.rs", ""),
        ]);
        let part = Part {
            name: "offers".to_owned(),
            module: None,
            paths: Vec::new(),
            words: vec!["offer".to_owned()],
        };

        let findings = leftovers(&repo, &[&part]);

        let places: Vec<(&str, usize)> = findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line))
            .collect();
        assert_eq!(places, [("src/offers_test.rs", 1), ("src/a.rs", 2)]);
    }

    #[test]
    fn arch_07_the_list_reads_the_parts_and_the_runs_and_an_empty_one_reads_as_none() {
        let repo = Repo::of(&[(
            LIST,
            "parts:\n  - name: offers\n    module: offers\n    paths:\n      - a\n    words:\n      - offer\nruns:\n  - name: shop\n    parts:\n",
        )]);

        let (parts, runs) = list(&repo).unwrap();

        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].module.as_deref(), Some("offers"));
        assert_eq!(runs[0].name, "shop");
        assert!(runs[0].parts.is_empty());
        let empty = Repo::of(&[(LIST, "parts:\nruns:\n")]);
        assert!(list(&empty).unwrap().0.is_empty());
    }
}
