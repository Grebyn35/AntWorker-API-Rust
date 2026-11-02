//! Removing an optional part, `just remove` (ARCH-07), and `check-removable`, which runs it on
//! copies of the repository, one for each run that `parts.yaml` lists, then `just checks` on each.

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

/// The documents that only the migration has, which describe the old API and its progress: the
/// migration's close deletes them, so a removal neither edits them nor reads them (DOC-04,
/// DOC-16).
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

/// Fails on each marker of the repository's files that names no part, and each block that no end
/// marker closes (ARCH-07).
fn check_markers(repo: &Repo, known: &[&str]) -> Vec<Finding> {
    repo.files()
        .filter(|file| !is_migration(&file.path))
        .filter_map(|file| {
            markers::owners(&file.text, known)
                .err()
                .map(|(line, message)| Finding::new(&file.path, line, message))
        })
        .collect()
}

/// The files that `part` owns whole, text or not: its paths, its module's migrations, and the
/// entries of the query cache whose queries name a table those migrations create.
fn owned(repo: &Repo, part: &Part) -> Vec<String> {
    let under = |path: &str, owner: &str| {
        path == owner || path.starts_with(&format!("{}/", owner.trim_end_matches('/')))
    };
    let mut owned: Vec<String> = repo
        .paths()
        .filter(|path| part.paths.iter().any(|owner| under(path, owner)))
        .map(str::to_owned)
        .collect();
    let Some(module) = &part.module else {
        return owned;
    };
    let migrations: Vec<&repo::File> = repo
        .files()
        .filter(|file| {
            file.path
                .strip_prefix("migrations/")
                .and_then(|name| name.split_once('_'))
                .is_some_and(|(stamp, rest)| {
                    stamp.chars().all(|c| c.is_ascii_digit())
                        && rest.starts_with(&format!("{module}_"))
                        && rest.ends_with(".sql")
                })
        })
        .collect();
    let tables: Vec<String> = migrations
        .iter()
        .flat_map(|file| created_tables(&file.text))
        .collect();
    owned.extend(migrations.iter().map(|file| file.path.clone()));
    owned.extend(
        repo.files()
            .filter(|file| file.path.starts_with(".sqlx/query-"))
            .filter(|file| {
                serde_json::from_str::<Value>(&file.text).is_ok_and(|entry| {
                    entry["query"]
                        .as_str()
                        .is_some_and(|query| tables.iter().any(|table| names(query, table)))
                })
            })
            .map(|file| file.path.clone()),
    );
    owned
}

/// The tables that the SQL of `migration` creates.
fn created_tables(migration: &str) -> Vec<String> {
    let words: Vec<&str> = migration.split_whitespace().collect();
    words
        .windows(3)
        .filter(|window| {
            window[0].eq_ignore_ascii_case("CREATE") && window[1].eq_ignore_ascii_case("TABLE")
        })
        .map(|window| {
            window[2]
                .trim_end_matches('(')
                .trim_matches('"')
                .to_ascii_lowercase()
        })
        .filter(|table| !table.eq_ignore_ascii_case("IF"))
        .collect()
}

/// Whether `sql` names `table` as a word of its own.
fn names(sql: &str, table: &str) -> bool {
    sql.match_indices(table).any(|(at, _)| {
        let before = sql[..at].chars().next_back();
        let after = sql[at + table.len()..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        !word(before) && !word(after)
    })
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
/// whole and the lines their markers mark, writes `openapi.json` and the metric catalog again,
/// formats the code, and fails while a file it kept still names one of them.
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
    let findings = check_markers(&repo, &known);
    if !findings.is_empty() {
        return failures("the markers are wrong, so nothing was removed", &findings);
    }
    let removed: Vec<&Part> = parts
        .iter()
        .filter(|part| names.contains(&part.name))
        .collect();
    let removing: Vec<&str> = removed.iter().map(|part| part.name.as_str()).collect();
    let deleted: Vec<String> = removed.iter().flat_map(|part| owned(&repo, part)).collect();
    for path in &deleted {
        if let Err(error) = repo::delete(path) {
            return fail(&format!("can't delete {path}: {error}"));
        }
    }
    let mut edited = 0;
    for file in repo
        .files()
        .filter(|file| !is_migration(&file.path) && !deleted.contains(&file.path))
    {
        match markers::strip(&file.text, &removing, &known) {
            Ok(Some(text)) => {
                if let Err(error) = repo::write(&file.path, &text) {
                    return fail(&format!("can't write {}: {error}", file.path));
                }
                edited += 1;
            }
            Ok(None) => {}
            Err((line, message)) => return fail(&format!("{}:{line}: {message}", file.path)),
        }
    }
    report(&format!(
        "removed {}: deleted {} files, and edited {edited}",
        removing.join(", "),
        deleted.len()
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

/// Writes `openapi.json` and the metric catalog again, from the code that the removal kept, and
/// formats the code.
fn regenerate() -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
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
    let known: Vec<&str> = parts.iter().map(|part| part.name.as_str()).collect();
    let findings = check_markers(&repo, &known);
    if !findings.is_empty() {
        return failures("the markers are wrong", &findings);
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

    #[test]
    fn arch_07_a_module_owns_its_migrations_and_the_cached_queries_of_their_tables() {
        let repo = Repo::of(&[
            ("crates/app/src/modules/offers/mod.rs", "mod repo;"),
            (
                "migrations/20260101000000_offers_offers.sql",
                "CREATE TABLE offers (\n  id uuid\n);",
            ),
            (
                "migrations/20260101000100_offers_more.sql",
                "ALTER TABLE offers ADD x int;",
            ),
            (
                "migrations/20260101000200_platform_jobs.sql",
                "CREATE TABLE jobs (id uuid);",
            ),
            (
                ".sqlx/query-1.json",
                r#"{"query": "SELECT id FROM offers WHERE id = $1"}"#,
            ),
            (
                ".sqlx/query-2.json",
                r#"{"query": "SELECT offers_count FROM jobs"}"#,
            ),
            ("docs/spec/offers.md", "# Spec"),
        ]);
        let part = Part {
            name: "offers".to_owned(),
            module: Some("offers".to_owned()),
            paths: vec![
                "crates/app/src/modules/offers".to_owned(),
                "docs/spec/offers.md".to_owned(),
            ],
            words: vec!["offer".to_owned()],
        };

        let mut owned = owned(&repo, &part);
        owned.sort();

        assert_eq!(
            owned,
            [
                ".sqlx/query-1.json",
                "crates/app/src/modules/offers/mod.rs",
                "docs/spec/offers.md",
                "migrations/20260101000000_offers_offers.sql",
                "migrations/20260101000100_offers_more.sql",
            ]
        );
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
