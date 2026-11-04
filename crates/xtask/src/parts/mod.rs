//! Removing an optional part, `just remove` (ARCH-07); `check-parts`, which runs each run's
//! removal in memory; and `check-removable`, which runs it on copies of the repository, one for
//! each run that `parts.yaml` lists, then `just checks` on each.

mod cache;
mod copy;
mod dry_run;
mod markers;

pub use dry_run::check;
pub use markers::unmarked;

use std::{path::Path, process::ExitCode};

use serde_json::Value;

use crate::{
    Finding, markdown, metrics,
    repo::{self, Repo},
};

/// The specs, in each of which a section that a removal empties reads "None." (DOC-08).
const SPECS: &str = "docs/spec/";

/// The list of the optional parts and of `check-removable`'s runs, from the repository root.
const LIST: &str = "crates/xtask/parts.yaml";

/// The migration workspace (DOC-13), which the migration's close deletes, so a removal neither
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
                             without the part (TEST-04)",
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

/// A text file that a removal kept: its path, and the lines it kept, each with its number in the
/// file, counted from 1.
type Kept<'a> = (&'a str, Vec<(usize, &'a str)>);

/// Each of `files`' lines that a removal kept, outside the migration's documents, that holds a
/// word of a removed part, and each of the kept `paths` that does, text or not.
fn leftovers<'a>(
    paths: impl Iterator<Item = &'a str>,
    files: impl Iterator<Item = Kept<'a>>,
    removed: &[&Part],
) -> Vec<Finding> {
    let words: Vec<(&str, String)> = removed
        .iter()
        .flat_map(|part| {
            part.words
                .iter()
                .map(|word| (part.name.as_str(), word.to_ascii_lowercase()))
        })
        .collect();
    let mut findings = Vec::new();
    for path in paths.filter(|path| !is_migration(path)) {
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
    for (path, lines) in files.filter(|(path, _)| !is_migration(path)) {
        for (number, line) in lines {
            let line = line.to_ascii_lowercase();
            for (part, word) in &words {
                if line.contains(word) {
                    findings.push(Finding::new(
                        path,
                        number,
                        format!("names `{part}`, which was removed (ARCH-07)"),
                    ));
                }
            }
        }
    }
    findings
}

/// Every line of `text`, with its number, counted from 1.
fn numbered(text: &str) -> Vec<(usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
        .collect()
}

/// Removes the parts named `names` from the repository (ARCH-07): it deletes the files they own
/// whole and the lines their markers mark, writing "None." in each section of a spec that it
/// empties, then the cached queries that no kept code holds, updates `Cargo.lock`, writes
/// `openapi.json` and the metric catalog again, formats the code, and fails while a file it kept,
/// or the path of one, still names one of them.
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
            let files = repo
                .files()
                .map(|file| (file.path.as_str(), numbered(&file.text)));
            let findings = leftovers(repo.paths(), files, &removed);
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
/// other file, but the migration's documents, writing "None." in each section of a spec that it
/// empties; returns how many files it deleted and edited.
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
            let text = if is_spec(&file.path) {
                none_in_empty_sections(&text)
            } else {
                text
            };
            repo::write(&file.path, &text)
                .map_err(|error| format!("can't write {}: {error}", file.path))?;
            edited += 1;
        }
    }
    Ok((deleted.len(), edited))
}

/// Whether the file at `path` is a module's spec, rather than the specs' README.
fn is_spec(path: &str) -> bool {
    path.strip_prefix(SPECS)
        .is_some_and(|name| name.ends_with(".md") && name != "README.md")
}

/// `text` with "None." under each of its level-2 headings that has nothing under it, as a spec
/// writes for a section with nothing in it (DOC-08).
fn none_in_empty_sections(text: &str) -> String {
    let empty: Vec<usize> = markdown::empty_sections(text)
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    let mut filled = String::with_capacity(text.len());
    let mut skipping = false;
    for (index, line) in text.lines().enumerate() {
        if skipping && line.trim().is_empty() {
            continue;
        }
        if skipping {
            filled.push('\n');
            skipping = false;
        }
        filled.push_str(line);
        filled.push('\n');
        if empty.contains(&(index + 1)) {
            filled.push_str("\nNone.\n");
            skipping = true;
        }
    }
    filled
}

/// Deletes the entries of the query cache whose query no kept code holds (DATA-14), and returns
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
                     #[tokio::test]\nasync fn platform_013_a_job_runs_for_its_scope() {}\n";
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
    }

    #[test]
    fn arch_07_a_test_s_module_is_the_words_before_its_behavior_s_number() {
        let names = ["client_telemetry_002_x()", "ops_15_a_restore()"];

        let modules: Vec<Option<String>> = names.iter().map(|name| behavior_module(name)).collect();

        assert_eq!(modules, [Some("client_telemetry".to_owned()), None]);
    }

    /// The part `offers`, which owns no file, with its word.
    fn offer_words() -> Part {
        Part {
            name: "offers".to_owned(),
            module: None,
            paths: Vec::new(),
            words: vec!["offer".to_owned()],
        }
    }

    /// Where `leftovers` finds a word of `offers` in `repo`'s files and paths.
    fn leftover_places(repo: &Repo) -> Vec<(String, usize)> {
        let files = repo
            .files()
            .map(|file| (file.path.as_str(), numbered(&file.text)));
        leftovers(repo.paths(), files, &[&offer_words()])
            .into_iter()
            .map(|finding| (finding.path, finding.line))
            .collect()
    }

    #[test]
    fn arch_07_a_removal_fails_on_each_kept_line_that_names_the_part() {
        let repo = Repo::of(&[("src/a.rs", "fn a() {}\n// The Offer's price.\n")]);

        let places = leftover_places(&repo);

        assert_eq!(places, [("src/a.rs".to_owned(), 2)]);
    }

    #[test]
    fn arch_07_a_removal_fails_on_each_kept_path_that_names_the_part() {
        let repo = Repo::of(&[("src/offers_test.rs", "")]);

        let places = leftover_places(&repo);

        assert_eq!(places, [("src/offers_test.rs".to_owned(), 1)]);
    }

    #[test]
    fn arch_07_a_removal_reads_no_word_in_the_migration_s_documents() {
        let repo = Repo::of(&[("docs/inventory/old.md", "The offers of the old API.")]);

        let places = leftover_places(&repo);

        assert!(places.is_empty(), "{places:?}");
    }

    #[test]
    fn arch_07_a_section_of_a_spec_that_the_removal_empties_reads_none() {
        let text =
            "# Spec\n\n## Interface\n\n\n## Routes\n\nA route.\n\n## Open design questions\n\n";

        let filled = none_in_empty_sections(text);

        assert_eq!(
            filled,
            "# Spec\n\n## Interface\n\nNone.\n\n## Routes\n\nA route.\n\n## Open design questions\n\nNone.\n"
        );
    }

    #[test]
    fn arch_07_the_list_reads_the_parts_and_the_runs() {
        let repo = Repo::of(&[(
            LIST,
            "parts:\n  - name: offers\n    module: offers\n    paths:\n      - a\n    words:\n      - offer\nruns:\n  - name: shop\n    parts:\n",
        )]);

        let (parts, runs) = list(&repo).unwrap();

        let read: Vec<(&str, Option<&str>)> = parts
            .iter()
            .map(|part| (part.name.as_str(), part.module.as_deref()))
            .chain(runs.iter().map(|run| (run.name.as_str(), None)))
            .collect();
        assert_eq!(read, [("offers", Some("offers")), ("shop", None)]);
    }

    #[test]
    fn arch_07_an_empty_list_reads_as_no_part() {
        let repo = Repo::of(&[(LIST, "parts:\nruns:\n")]);

        let (parts, _) = list(&repo).unwrap();

        assert!(parts.is_empty());
    }
}
