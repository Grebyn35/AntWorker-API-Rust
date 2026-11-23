//! Removing optional parts (ARCH-07): what `just remove` and `just keep` share, which `remove`
//! holds with them and with `check-removable`, and `check-parts`, which does in memory each run's
//! removal, as the run's task makes it, and each part's alone.

mod asvs;
mod cache;
mod citations;
mod dry_run;
mod exports;
mod imports;
mod markers;
mod matrix;
mod remove;
mod validate;

pub use dry_run::check;
pub use markers::unmarked;
pub use remove::{check_removable, keep, remove};
use validate::validate;

use std::path::Path;

use serde_json::Value;

use crate::{
    Finding, catalog, copy, markdown, metrics, migrations,
    repo::{self, Repo},
};

/// The specs, in each of which a section that a removal empties reads "None." (DOC-08).
const SPECS: &str = "docs/spec/";

/// The list of the optional parts and of `check-removable`'s runs, from the repository root.
const LIST: &str = "crates/xtask/parts.yaml";

/// The migration workspace (DOC-13), which the migration's close deletes, so a removal reads no
/// word of a part in it, but deletes there what a part's markers mark, such as its decisions
/// (ARCH-07).
const MIGRATION: [&str; 2] = ["docs/inventory/", "docs/MIGRATION.md"];

/// The part that holds what makes the others removable, which `just keep` removes with the parts
/// that a fork doesn't keep, and `just remove` never removes.
const REMOVAL: &str = "removal";

/// An optional part, as `parts.yaml` lists it.
struct Part {
    name: String,
    module: Option<String>,
    paths: Vec<String>,
    words: Vec<String>,
    /// Whether it is a part that creates workspaces, of which a fork keeps one at least.
    creates_workspaces: bool,
}

/// A run of `check-removable`: the parts it removes from a copy of its own.
struct Run {
    name: String,
    parts: Vec<String>,
    /// Whether it removes them with `just remove`, which keeps `removal`, as a fork that has yet
    /// to choose its parts does, rather than with `just keep`.
    keeps_removal: bool,
}

/// The parts and the runs that `parts.yaml` lists. Fails on what it can't read as what its key
/// holds: `parts`, `runs`, a part's `paths` or `words`, or a run's `parts`, that isn't a list; an
/// entry of one of the last three that isn't a string, as YAML reads a word that ends in a colon
/// unless it is quoted; a part or a run without a `name`; a `name` or a `module` that isn't a
/// string; a `creates_workspaces` that isn't a boolean; a run's `task` other than `remove`; and a
/// key that the file, a part or a run doesn't have, as a misspelled one.
fn list(repo: &Repo) -> Result<(Vec<Part>, Vec<Run>), String> {
    let file = repo
        .file(LIST)
        .ok_or_else(|| format!("{LIST} is missing"))?;
    let value: Value =
        serde_saphyr::from_str(&file.text).map_err(|error| format!("{LIST}: {error}"))?;
    known_keys(&value, &["parts", "runs"], "the file")?;
    let mut parts = Vec::new();
    for part in sequence(&value, "parts", "the file")? {
        let name = named(part, "part")?;
        let of = format!("`{name}`");
        known_keys(
            part,
            &["name", "module", "paths", "words", "creates_workspaces"],
            &of,
        )?;
        let creates_workspaces = match &part["creates_workspaces"] {
            Value::Null => false,
            Value::Bool(creates) => *creates,
            _ => {
                return Err(format!(
                    "{LIST}: `creates_workspaces` of {of} isn't `true` or `false`"
                ));
            }
        };
        parts.push(Part {
            module: string(part, "module", &of)?,
            paths: strings(part, "paths", &of)?,
            words: strings(part, "words", &of)?,
            creates_workspaces,
            name,
        });
    }
    let mut runs = Vec::new();
    for run in sequence(&value, "runs", "the file")? {
        let name = named(run, "run")?;
        known_keys(run, &["name", "task", "parts"], &format!("`{name}`"))?;
        let keeps_removal = match &run["task"] {
            Value::Null => false,
            Value::String(task) if task == "remove" => true,
            _ => return Err(format!("{LIST}: the `task` of `{name}` isn't `remove`")),
        };
        runs.push(Run {
            parts: strings(run, "parts", &format!("`{name}`"))?,
            keeps_removal,
            name,
        });
    }
    Ok((parts, runs))
}

/// Fails on a key of `entry` that isn't one of `keys`. `of` names the entry.
fn known_keys(entry: &Value, keys: &[&str], of: &str) -> Result<(), String> {
    let unknown = entry
        .as_object()
        .and_then(|entry| entry.keys().find(|key| !keys.contains(&key.as_str())));
    match unknown {
        Some(key) => Err(format!(
            "{LIST}: `{key}` of {of} is none of its keys, which are {}",
            keys.join(", ")
        )),
        None => Ok(()),
    }
}

/// The name of `entry`, a part or a run, as `kind` says, or why it has none.
fn named(entry: &Value, kind: &str) -> Result<String, String> {
    string(entry, "name", &format!("a {kind}"))?
        .ok_or_else(|| format!("{LIST}: a {kind} has no `name`"))
}

/// The string that `entry` holds under `key`, if it holds anything there, or why what it holds
/// is no string. `of` names the entry.
fn string(entry: &Value, key: &str, of: &str) -> Result<Option<String>, String> {
    match &entry[key] {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        other => Err(format!("{LIST}: `{key}` of {of} isn't a string, {other}")),
    }
}

/// The entries that `entry` lists under `key`, none if it holds nothing there, or why what it
/// holds is no list. `of` names the entry.
fn sequence<'a>(entry: &'a Value, key: &str, of: &str) -> Result<&'a [Value], String> {
    match &entry[key] {
        Value::Null => Ok(&[]),
        Value::Array(items) => Ok(items),
        other => Err(format!(
            "{LIST}: `{key}` of {of} isn't a list, {other}: write each entry on a line of its \
             own, after `- `"
        )),
    }
}

/// The strings that `entry` lists under `key`, or why one of them is none. `of` names the entry.
fn strings(entry: &Value, key: &str, of: &str) -> Result<Vec<String>, String> {
    sequence(entry, key, of)?
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                format!("{LIST}: an entry of `{key}` of {of} isn't a string, {item}: quote it")
            })
        })
        .collect()
}

/// Whether `path` is one of the migration's own documents.
fn is_migration(path: &str) -> bool {
    MIGRATION.iter().any(|kept| path.starts_with(kept))
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
                .filter(|path| migrations::of_module(path, module))
                .map(str::to_owned),
        );
    }
    owned
}

/// A text file that a removal kept: its path, and the lines it kept, each with its number in the
/// file, counted from 1.
type Kept<'a> = (&'a str, Vec<(usize, &'a str)>);

/// Each of `files`' lines that a removal kept, outside the migration's documents, that holds a
/// word of a removed part, or ends one that the line before began, and each of the kept `paths`
/// that holds one, text or not. A line break divides a word between two kept lines, as a sentence
/// that the break divides reads, where it stands for one of the word's spaces: the word's text
/// before that space ends the first line, without the white space that ends it, and its text
/// after the space begins the next, as [`continued`] gives it.
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
    // Each word that holds a space, with its text before and after that space, for each of its
    // spaces: a line break may stand for it.
    let divided: Vec<(&str, &str, &str, &str)> = words
        .iter()
        .flat_map(|(part, word)| {
            word.match_indices(' ')
                .map(move |(at, _)| (*part, word.as_str(), &word[..at], &word[at + 1..]))
        })
        .collect();
    for (path, lines) in files.filter(|(path, _)| !is_migration(path)) {
        let mut before = String::new();
        for (number, line) in lines {
            let line = line.to_ascii_lowercase();
            let mut names = |part: &str| {
                findings.push(Finding::new(
                    path,
                    number,
                    format!("names `{part}`, which was removed (ARCH-07)"),
                ));
            };
            for (part, word) in &words {
                if line.contains(word) {
                    names(part);
                }
            }
            let (ended, begun) = (before.trim_end(), continued(&line));
            for (part, word, start, rest) in &divided {
                if ended.ends_with(start) && begun.starts_with(rest) && !line.contains(word) {
                    names(part);
                }
            }
            before = line;
        }
    }
    findings
}

/// What starts a comment that runs to its line's end, in the files that hold one, `///` and `//!`
/// before `//`, which starts them both.
const COMMENTS: [&str; 6] = ["///", "//!", "//", "#", "--", "%%"];

/// `line` without the white space and the start of a comment that it begins with.
fn continued(line: &str) -> &str {
    let line = line.trim_start();
    COMMENTS
        .iter()
        .find_map(|start| line.strip_prefix(start))
        .unwrap_or(line)
        .trim_start()
}

/// Every line of `text`, with its number, counted from 1.
fn numbered(text: &str) -> Vec<(usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
        .collect()
}

/// Deletes the files that the parts `removed` own whole, and the lines their markers mark in every
/// other file, with a blank line where [`markers::strip`] says, writing "None." in each section of
/// a spec that it empties, retiring the numbers of the behaviors it deletes from one, and keeping
/// in the security map a row of each requirement, as [`asvs::keeping`] says; returns how many
/// files it deleted and edited.
fn delete(repo: &Repo, removed: &[&Part], known: &[&str]) -> Result<(usize, usize), String> {
    let removing: Vec<&str> = removed.iter().map(|part| part.name.as_str()).collect();
    let deleted: Vec<String> = removed.iter().flat_map(|part| owned(repo, part)).collect();
    for path in &deleted {
        repo::delete(path).map_err(|error| format!("can't delete {path}: {error}"))?;
    }
    let mut edited = 0;
    for file in repo.files().filter(|file| !deleted.contains(&file.path)) {
        let kept = (file.path == asvs::MAP)
            .then(|| asvs::keeping(&file.text, &removing, known))
            .flatten();
        let source = kept.as_deref().unwrap_or(&file.text);
        let stripped = markers::strip(&file.path, source, &removing, known)
            .map_err(|(line, message)| format!("{}:{line}: {message}", file.path))?;
        if let Some(text) = stripped.or(kept) {
            let text = if is_spec(&file.path) {
                none_in_empty_sections(&retire_removed(&file.text, &text))
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

/// `kept`, a spec that a removal stripped from `original`, with the number of each behavior that
/// it deleted retired, in the line of retired IDs at the top of its Behaviors, so no behavior the
/// fork adds takes it (DOC-08).
fn retire_removed(original: &str, kept: &str) -> String {
    let left = behavior_numbers(kept);
    let mut removed: Vec<u32> = behavior_numbers(original)
        .into_iter()
        .filter(|number| !left.contains(number))
        .collect();
    if removed.is_empty() {
        return kept.to_owned();
    }
    let mut lines: Vec<String> = kept.lines().map(str::to_owned).collect();
    let Some(heading) = lines.iter().position(|line| line == "## Behaviors") else {
        return kept.to_owned();
    };
    let existing = lines
        .iter()
        .enumerate()
        .skip(heading + 1)
        .take_while(|(_, line)| !line.starts_with("## "))
        .find_map(|(index, line)| catalog::retired_numbers(line).map(|numbers| (index, numbers)));
    if let Some((_, numbers)) = &existing {
        removed.extend(numbers);
    }
    removed.sort_unstable();
    removed.dedup();
    let line = retired_line(&removed);
    match existing {
        Some((index, _)) => lines[index] = line,
        None => {
            lines.insert(heading + 1, String::new());
            lines.insert(heading + 2, line);
            // One blank line between the new line and what follows it.
            while lines.get(heading + 3).is_some_and(String::is_empty)
                && lines.get(heading + 4).is_some_and(String::is_empty)
            {
                lines.remove(heading + 4);
            }
        }
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

/// The numbers of the behaviors of a spec's table: the three digits of each row's ID.
fn behavior_numbers(text: &str) -> Vec<u32> {
    text.lines()
        .filter_map(|line| line.strip_prefix("| "))
        .filter_map(|row| row.split_once(" |").map(|(id, _)| id))
        .filter_map(|id| id.rsplit_once('-'))
        .filter(|(prefix, digits)| {
            !prefix.is_empty()
                && prefix.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                && digits.len() == 3
        })
        .filter_map(|(_, digits)| digits.parse().ok())
        .collect()
}

/// The line that retires `numbers`, such as `IDs 008 and 011 are retired.` (DOC-08).
fn retired_line(numbers: &[u32]) -> String {
    let listed: Vec<String> = numbers
        .iter()
        .map(|number| format!("{number:03}"))
        .collect();
    match listed.as_slice() {
        [one] => format!("ID {one} is retired."),
        [rest @ .., last] => format!("IDs {} and {last} are retired.", rest.join(", ")),
        [] => String::new(),
    }
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
            creates_workspaces: false,
        }
    }

    #[test]
    fn arch_07_a_removal_retires_the_numbers_of_the_behaviors_it_deletes() {
        let original = "# Spec\n\n## Behaviors\n\n| ID | Behavior |\n|---|---|\n\
                        | OFFERS-001 | One |\n| OFFERS-002 | Two |\n| OFFERS-003 | Three |\n\n## Design\n";
        let kept = "# Spec\n\n## Behaviors\n\n| ID | Behavior |\n|---|---|\n\
                    | OFFERS-001 | One |\n| OFFERS-003 | Three |\n\n## Design\n";
        assert_eq!(
            retire_removed(original, kept),
            "# Spec\n\n## Behaviors\n\nID 002 is retired.\n\n| ID | Behavior |\n|---|---|\n\
             | OFFERS-001 | One |\n| OFFERS-003 | Three |\n\n## Design\n"
        );
        let retired = "# Spec\n\n## Behaviors\n\nID 004 is retired.\n\n| ID | Behavior |\n\
                       |---|---|\n| OFFERS-001 | One |\n| OFFERS-002 | Two |\n| OFFERS-005 | Five |\n";
        let kept = "# Spec\n\n## Behaviors\n\nID 004 is retired.\n\n| ID | Behavior |\n\
                    |---|---|\n| OFFERS-005 | Five |\n";
        assert_eq!(
            retire_removed(retired, kept),
            "# Spec\n\n## Behaviors\n\nIDs 001, 002 and 004 are retired.\n\n| ID | Behavior |\n\
             |---|---|\n| OFFERS-005 | Five |\n"
        );
        assert_eq!(retire_removed(kept, kept), kept);
        // A block that the removal deleted leaves no second blank line.
        let original = "## Behaviors\n\n| ID | Behavior |\n|---|---|\n| OFFERS-001 | One |\n";
        let kept = "## Behaviors\n\n\n### Area\n";
        assert_eq!(
            retire_removed(original, kept),
            "## Behaviors\n\nID 001 is retired.\n\n### Area\n"
        );
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
        let names = ["order_items_002_x()", "ops_15_a_restore()"];

        let behaviors: Vec<Option<(String, String)>> =
            names.iter().map(|name| validate::behavior(name)).collect();

        assert_eq!(
            behaviors,
            [Some(("order_items".to_owned(), "002".to_owned())), None]
        );
    }

    #[test]
    fn arch_07_a_part_s_file_may_hold_the_test_of_another_spec_s_behavior_that_goes_with_it() {
        let tests = "#[tokio::test]\nasync fn platform_013_a_job_runs() {}\n\
                     #[tokio::test]\nasync fn platform_014_an_offer_s_job_runs() {}\n";
        // The spec's marker is built, so the source of these tests holds none.
        let spec = format!(
            "| PLATFORM-013 | A job runs. | | `platform_013_` |\n\
             | PLATFORM-014 | An offer's job runs. | | `platform_014_` <!-- {} offers --> |\n",
            "part:"
        );
        let repo = Repo::of(&[
            (LIST, "parts:\n"),
            ("crates/app/tests/integration/offers.rs", tests),
            ("docs/spec/platform.md", spec.as_str()),
        ]);

        let findings = validate(&repo, &[offers()]);

        let places: Vec<(&str, usize)> = findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line))
            .collect();
        assert_eq!(places, [("crates/app/tests/integration/offers.rs", 2)]);
    }

    #[test]
    fn arch_07_a_part_s_file_may_hold_in_another_part_s_block_the_test_of_that_part_s_behavior() {
        // The markers are built, so the source of these tests holds none.
        let open = |part: &str| format!("// {} {part}", "part:");
        let close = |part: &str| format!("// end {} {part}", "part:");
        let tests = [
            open("coupons"),
            "#[tokio::test]".to_owned(),
            "async fn platform_013_an_offer_takes_a_coupon() {}".to_owned(),
            "#[tokio::test]".to_owned(),
            "async fn platform_014_an_offer_takes_a_gift() {}".to_owned(),
            "#[tokio::test]".to_owned(),
            "async fn platform_015_an_offer_runs() {}".to_owned(),
            close("coupons"),
            "#[tokio::test]".to_owned(),
            "async fn platform_013_an_offer_takes_a_coupon_outside_its_block() {}".to_owned(),
            String::new(),
        ]
        .join("\n");
        let spec = format!(
            "| PLATFORM-013 | A coupon applies. | | `platform_013_` <!-- {0} coupons --> |\n\
             | PLATFORM-014 | A gift applies. | | `platform_014_` <!-- {0} gifts --> |\n\
             | PLATFORM-015 | A job runs. | | `platform_015_` |\n",
            "part:"
        );
        let repo = Repo::of(&[
            (LIST, "parts:\n"),
            ("crates/app/tests/integration/offers.rs", tests.as_str()),
            ("docs/spec/platform.md", spec.as_str()),
        ]);
        let unowned = |name: &str| Part {
            name: name.to_owned(),
            ..offer_words()
        };

        let findings = validate(&repo, &[offers(), unowned("coupons"), unowned("gifts")]);

        // The test in the block of `coupons`, whose marker the row carries, goes with either
        // part; the others go with `offers` while their behaviors stay.
        let places: Vec<usize> = findings.iter().map(|finding| finding.line).collect();
        assert_eq!(places, [5, 7, 10], "{findings:?}");
    }

    #[test]
    fn arch_07_a_test_in_a_file_of_a_part_without_a_module_is_checked_as_well() {
        let tests = "#[tokio::test]\nasync fn auth_001_a_user_registers() {}\n\
                     #[tokio::test]\nasync fn auth_002_a_coupon_signs_in() {}\n";
        let spec = format!(
            "| AUTH-001 | A user registers. | | `auth_001_` |\n\
             | AUTH-002 | A coupon signs in. | | `auth_002_` <!-- {} coupons --> |\n",
            "part:"
        );
        let repo = Repo::of(&[
            (LIST, "parts:\n"),
            ("crates/app/tests/integration/auth/coupons.rs", tests),
            ("docs/spec/auth.md", spec.as_str()),
        ]);
        let coupons = Part {
            name: "coupons".to_owned(),
            module: None,
            paths: vec!["crates/app/tests/integration/auth/coupons.rs".to_owned()],
            words: vec!["coupon".to_owned()],
            creates_workspaces: false,
        };

        let findings = validate(&repo, &[coupons]);

        let places: Vec<(&str, usize)> = findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line))
            .collect();
        assert_eq!(
            places,
            [("crates/app/tests/integration/auth/coupons.rs", 2)]
        );
    }

    /// The part `offers`, which owns no file, with its word.
    fn offer_words() -> Part {
        Part {
            name: "offers".to_owned(),
            module: None,
            paths: Vec::new(),
            words: vec!["offer".to_owned()],
            creates_workspaces: false,
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
    fn arch_07_the_migration_s_documents_hold_markers_that_name_parts() {
        // The markers are built, so the source of these tests holds none.
        let tracker = format!(
            "| D1 | An offer's decision. <!-- {0} offers --> |\n| D2 | Another. <!-- {0} gone --> |\n",
            "part:"
        );
        let repo = Repo::of(&[(LIST, "parts:\n"), ("docs/MIGRATION.md", tracker.as_str())]);

        let findings = validate(&repo, &[offers()]);
        let owners = markers::owners(&tracker, &["offers", "gone"]).unwrap();
        let kept = markers::kept(&tracker, &owners, &["offers"]);

        let places: Vec<usize> = findings.iter().map(|finding| finding.line).collect();
        assert_eq!(places, [2]);
        assert_eq!(kept.len(), 1, "{kept:?}");
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

    #[test]
    fn arch_07_the_list_refuses_an_entry_that_yaml_reads_as_no_string() {
        for (list_of, text) in [
            (
                "words",
                "parts:\n  - name: offers\n    words:\n      - offers::\n",
            ),
            ("paths", "parts:\n  - name: offers\n    paths:\n      - 7\n"),
            (
                "parts",
                "runs:\n  - name: offers\n    parts:\n      - offers:\n",
            ),
        ] {
            let error = list(&Repo::of(&[(LIST, text)])).err().unwrap();

            assert!(
                error.contains(&format!(
                    "an entry of `{list_of}` of `offers` isn't a string"
                )),
                "{error}"
            );
        }
        let quoted = "parts:\n  - name: offers\n    words:\n      - \"offers::\"\n";
        let (parts, _) = list(&Repo::of(&[(LIST, quoted)])).unwrap();
        assert_eq!(parts[0].words, ["offers::"]);
    }

    #[test]
    fn arch_07_the_list_refuses_a_list_or_a_name_that_it_can_t_read() {
        for (text, refusal) in [
            (
                "parts:\n  - name: offers\n    words: offer\n",
                "`words` of `offers` isn't a list",
            ),
            (
                "parts:\n  - name: offers\n    paths: src/offers\n",
                "`paths` of `offers` isn't a list",
            ),
            (
                "runs:\n  - name: shop\n    parts: offers\n",
                "`parts` of `shop` isn't a list",
            ),
            ("parts: offers\n", "`parts` of the file isn't a list"),
            ("runs:\n  name: shop\n", "`runs` of the file isn't a list"),
            (
                "parts:\n  - words:\n      - offer\n",
                "a part has no `name`",
            ),
            ("runs:\n  - parts:\n      - offers\n", "a run has no `name`"),
            ("parts:\n  - name: 7\n", "`name` of a part isn't a string"),
            (
                "parts:\n  - name: offers\n    word:\n      - offer\n",
                "`word` of `offers` is none of its keys",
            ),
            (
                "runs:\n  - name: shop\n    part:\n      - offers\n",
                "`part` of `shop` is none of its keys",
            ),
            (
                "part:\n  - name: offers\n",
                "`part` of the file is none of its keys",
            ),
            (
                "parts:\n  - name: offers\n    module:\n      - offers\n",
                "`module` of `offers` isn't a string",
            ),
        ] {
            let error = list(&Repo::of(&[(LIST, text)])).err().unwrap();

            assert!(error.contains(refusal), "{text}: {error}");
        }
    }

    #[test]
    fn arch_07_a_run_takes_the_parts_of_another_by_its_anchor() {
        let text = "runs:\n  - name: few\n    parts: &few\n      - offers\n      - coupons\n  \
                    - name: same\n    task: remove\n    parts: *few\n";

        let (_, runs) = list(&Repo::of(&[(LIST, text)])).unwrap();

        assert_eq!(runs[0].parts, ["offers", "coupons"]);
        assert_eq!(runs[1].parts, runs[0].parts);
        assert!(runs[1].keeps_removal && !runs[0].keeps_removal);
        // Once every part of the list is removed, both runs remove none.
        let text = "runs:\n  - name: few\n    parts: &few\n  - name: same\n    task: remove\n    \
                    parts: *few\n";
        let (_, runs) = list(&Repo::of(&[(LIST, text)])).unwrap();
        assert!(runs[0].parts.is_empty() && runs[1].parts.is_empty());
    }

    #[test]
    fn arch_07_the_list_refuses_a_task_or_a_creator_that_it_can_t_read() {
        let runs = |task: &str| format!("runs:\n  - name: shop\n    task: {task}\n");
        let (_, read) = list(&Repo::of(&[(LIST, runs("remove").as_str())])).unwrap();
        assert!(read[0].keeps_removal);
        let (_, read) = list(&Repo::of(&[(LIST, "runs:\n  - name: shop\n")])).unwrap();
        assert!(!read[0].keeps_removal);
        for task in ["keep", "true", "[remove]"] {
            let error = list(&Repo::of(&[(LIST, runs(task).as_str())]))
                .err()
                .unwrap();
            assert!(
                error.ends_with("the `task` of `shop` isn't `remove`"),
                "{error}"
            );
        }

        let parts = |creates: &str| {
            format!("parts:\n  - name: offers\n    creates_workspaces: {creates}\n")
        };
        let (read, _) = list(&Repo::of(&[(LIST, parts("true").as_str())])).unwrap();
        assert!(read[0].creates_workspaces);
        let error = list(&Repo::of(&[(LIST, parts("offers").as_str())]))
            .err()
            .unwrap();
        assert!(
            error.contains("`creates_workspaces` of `offers`"),
            "{error}"
        );
    }

    #[test]
    fn arch_07_a_removal_fails_on_a_word_that_a_line_break_divides() {
        let part = Part {
            words: vec!["special offer".to_owned(), "just offer ".to_owned()],
            ..offer_words()
        };
        let places = |text: &str| -> Vec<usize> {
            let repo = Repo::of(&[("src/a.rs", text)]);
            let files = repo
                .files()
                .map(|file| (file.path.as_str(), numbered(&file.text)));
            leftovers(repo.paths(), files, &[&part])
                .into_iter()
                .map(|finding| finding.line)
                .collect()
        };

        // The word's end is found on the line that holds it, after its comment's start.
        assert_eq!(places("/// Every special\n    /// offer goes.\n"), [2]);
        assert_eq!(places("# A Special  \n# Offer\n"), [2]);
        assert_eq!(places("Run `just offer`: just offer\nthem.\n"), [2]);
        // A word on one line is found once, and words that only stand side by side are none.
        assert_eq!(places("A special offer\nfollows.\n"), [1]);
        assert!(places("// It is special.\n// Offer nothing.\n").is_empty());
        assert!(places("special\n\noffer\n").is_empty());
    }
}
