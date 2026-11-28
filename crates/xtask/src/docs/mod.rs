//! `check-docs`: links to the repository's files, IDs and the CI checks the rules name all
//! resolve, and the row of such a check lists each rule whose `Check` names it (DOC-12, in
//! `enforced`); prose and comments hold no history markers (DOC-01) and no unfinished-work
//! markers (DOC-04); every document belongs to the documentation set (DOC-05); documents spell
//! out acronyms where they first use them (DOC-06, in `acronyms`), and hold no sentence,
//! paragraph or cell of more words than its bound (DOC-15, in `words`); specs, rules, tables and
//! the tracker keep their formats (DOC-08 and DOC-14, in `formats`); each spec's Routes table
//! matches `openapi.json` (DOC-08, in `routes`); the accounts spec's tables of the personal data
//! contracts match the contracts that the composition root registers (DOC-07, in `contracts`);
//! and test prefixes, paths to Rust files, the name of a codebase this one replaces, and paths
//! into the inventory are named only where they resolve or DOC-13 allows (`references`); and two
//! lists that documents keep of the code hold what the code holds (DOC-01, in `listings`).

mod enforced;
mod listings;

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Finding, Outcome, acronyms, catalog::Catalog, contracts, formats, markdown, references,
    repo::Repo, routes, text, words,
};
use listings::{composition, subcommands};

/// Words that describe history instead of the current state (DOC-01).
const HISTORY: [&str; 5] = ["previously", "no longer", "used to", "legacy", "now uses"];

/// Words that mark unfinished work (DOC-04).
pub const UNFINISHED: [&str; 3] = ["TODO", "FIXME", "XXX"];

/// The migration workspace's inventory (DOC-13), which numbers its entries with IDs of its own.
/// No ID, marker or comment in it is checked.
const INVENTORY: &str = "docs/inventory/";

/// The migration tracker, the other part of the migration workspace (DOC-13).
const TRACKER: &str = "docs/MIGRATION.md";

/// The release notes for forks, one of history's homes (DOC-01, DOC-11).
const CHANGELOG: &str = "CHANGELOG.md";

/// The architecture decision records, another of history's homes (DOC-01, DOC-10).
const ADRS: &str = "docs/adr/";

/// The API's document, which holds the strings of each route's `#[utoipa::path]`, which no
/// comment holds (DOC-01).
const DOCUMENT: &str = "openapi.json";

/// Runs the check.
pub fn check(repo: &Repo, catalog: &Catalog) -> Outcome {
    let documents: BTreeMap<&str, markdown::Document> = repo
        .files()
        .filter(|file| file.path.ends_with(".md"))
        .map(|file| (file.path.as_str(), markdown::read(&file.text)))
        .collect();
    let mut findings = catalog.findings().to_vec();
    findings.extend(links(repo, &documents));
    findings.extend(comment_links(repo, &documents));
    findings.extend(headings(&documents));
    findings.extend(ids(repo, catalog));
    findings.extend(named_checks(repo, catalog));
    findings.extend(enforced::unlisted(repo));
    findings.extend(subcommands(repo));
    findings.extend(composition(repo));
    findings.extend(prose_markers(&documents));
    findings.extend(comment_markers(repo));
    findings.extend(document_markers(repo));
    findings.extend(locations(repo));
    findings.extend(formats::check(repo));
    findings.extend(routes::check(repo));
    findings.extend(acronyms::check(repo, catalog));
    findings.extend(acronyms::unused(repo));
    findings.extend(references::check(repo, catalog));
    findings.extend(contracts::check(repo));
    findings.extend(words::check(repo));
    Outcome {
        report: vec![format!(
            "check-docs: {} files checked, {} of them documents.",
            repo.files().count(),
            documents.len()
        )],
        findings,
    }
}

/// No two headings of a document share their text, so each anchor names one heading (DOC-12),
/// but in the inventory and in the changelog, whose sections each repeat the headings of Keep a
/// Changelog (DOC-11): a link to one of those that [`unresolved`] reads fails instead.
fn headings(documents: &BTreeMap<&str, markdown::Document>) -> Vec<Finding> {
    documents
        .iter()
        .filter(|(path, _)| !path.starts_with(INVENTORY) && **path != CHANGELOG)
        .flat_map(|(path, document)| {
            document.repeated_headings.iter().map(|(line, heading)| {
                Finding::new(
                    path,
                    *line,
                    format!(
                        "an earlier heading also reads `{heading}`, so their anchors follow \
                         their order: give each heading a text of its own (DOC-12)"
                    ),
                )
            })
        })
        .collect()
}

/// Every link to a file of the repository resolves, and so does its anchor (DOC-12).
fn links(repo: &Repo, documents: &BTreeMap<&str, markdown::Document>) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (path, document) in documents {
        for link in &document.links {
            if let Some(problem) = unresolved(repo, documents, path, &link.destination) {
                findings.push(Finding::new(
                    path,
                    link.line,
                    format!(
                        "the link to `{}` doesn't resolve: {problem} (DOC-12)",
                        link.destination
                    ),
                ));
            }
        }
    }
    findings
}

/// Every link of a Rust file's doc comments to a file resolves, as a document's does (DOC-12),
/// but in a check's own tests (`text::citing_lines`). A link to an item, such as
/// `[cfg](super::cfg)`, is rustdoc's, which `just check-doc-links` has resolve it, and a link
/// inside a code span is no link.
fn comment_links(repo: &Repo, documents: &BTreeMap<&str, markdown::Document>) -> Vec<Finding> {
    let mut findings = Vec::new();
    for file in repo.files().filter(|file| file.path.ends_with(".rs")) {
        let citing = text::citing_lines(&file.path, &file.text);
        for (index, line) in file.text.lines().take(citing).enumerate() {
            for destination in comment_link_destinations(line) {
                if let Some(problem) = unresolved(repo, documents, &file.path, &destination) {
                    findings.push(Finding::new(
                        &file.path,
                        index + 1,
                        format!("the link to `{destination}` doesn't resolve: {problem} (DOC-12)"),
                    ));
                }
            }
        }
    }
    findings
}

/// The destinations of the links of `line`, a line of a Rust file, that `check-docs` resolves:
/// those of a doc comment's prose, outside its code spans, but a link to an item, such as
/// `[cfg](super::cfg)`, which is rustdoc's.
pub fn comment_link_destinations(line: &str) -> Vec<String> {
    let comment = line.trim_start();
    if !(comment.starts_with("///") || comment.starts_with("//!")) {
        return Vec::new();
    }
    let prose = comment.split('`').step_by(2).collect::<Vec<_>>().join(" ");
    prose
        .split("](")
        .skip(1)
        .filter_map(|rest| rest.split_once(')').map(|(destination, _)| destination))
        .filter(|destination| !destination.contains("::"))
        .map(str::to_owned)
        .collect()
}

/// What a link to `destination`, in the file at `from`, points at in the repository: a path,
/// `from` where it names only an anchor, and the anchor, empty where it names none. `None` for a
/// link to another site, and for a path that climbs out of the repository.
pub fn link_target<'a>(from: &str, destination: &'a str) -> Option<(String, &'a str)> {
    if destination.contains("://") || destination.starts_with("mailto:") {
        return None;
    }
    let (target, anchor) = destination.split_once('#').unwrap_or((destination, ""));
    if target.is_empty() {
        return Some((from.to_owned(), anchor));
    }
    let parent = from.rsplit_once('/').map_or("", |(directory, _)| directory);
    Some((join(parent, target)?, anchor))
}

/// Why the link to `destination`, in the document at `from`, doesn't resolve. Links to other
/// sites aren't checked.
fn unresolved(
    repo: &Repo,
    documents: &BTreeMap<&str, markdown::Document>,
    from: &str,
    destination: &str,
) -> Option<String> {
    if destination.contains("://") || destination.starts_with("mailto:") {
        return None;
    }
    let Some((target, anchor)) = link_target(from, destination) else {
        return Some("it points outside the repository".to_owned());
    };
    if !repo.has(&target) {
        return Some(format!("there is no `{target}`"));
    }
    match documents.get(target.as_str()) {
        Some(document) if !anchor.is_empty() && !document.anchors.contains(anchor) => Some(
            format!("`{target}` has no heading whose anchor is `#{anchor}`"),
        ),
        Some(document) if document.repeated.contains(anchor) => Some(format!(
            "another heading of `{target}` has the text of the one `#{anchor}` names, so the \
             anchor follows their order: give each heading a text of its own"
        )),
        _ => None,
    }
}

/// `relative` resolved against `directory`, or `None` if it climbs out of the repository.
fn join(directory: &str, relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// Every rule or behavior ID cited outside the inventory, the query cache in `.sqlx/`, which
/// sqlx-cli writes from the queries of the code, where the check reads them, and `Cargo.lock`,
/// which Cargo writes, and outside a check's own tests (`text::citing_lines`), exists (DOC-12).
/// A token whose prefix no rule or module uses, such as `SHA-256`, isn't an ID.
fn ids(repo: &Repo, catalog: &Catalog) -> Vec<Finding> {
    let mut findings = Vec::new();
    let cited = repo.files().filter(|file| {
        !file.path.starts_with(INVENTORY)
            && !file.path.starts_with(".sqlx/")
            && file.path != "Cargo.lock"
    });
    for file in cited {
        let citing = text::citing_lines(&file.path, &file.text);
        for (index, line) in file.text.lines().take(citing).enumerate() {
            for token in text::id_tokens(line) {
                let id = token.id();
                if catalog.knows_prefix(token.prefix) && !catalog.defines(&id) {
                    findings.push(Finding::new(
                        &file.path,
                        index + 1,
                        format!("{id} isn't a rule or behavior ID that exists (DOC-12)"),
                    ));
                }
            }
        }
    }
    findings
}

/// Every CI check a rule names is listed: in the task column of the table of checks in
/// `docs/operations/ci.md` once it is built, and in the first column of the tracker's table of
/// CI checks until then (DOC-12).
fn named_checks(repo: &Repo, catalog: &Catalog) -> Vec<Finding> {
    let mut listed = BTreeSet::new();
    if let Some(ci) = repo.file("docs/operations/ci.md") {
        listed.extend(column(&ci.text, "Checks that exist", |header| {
            header.contains("task")
        }));
    }
    if let Some(tracker) = repo.file(TRACKER) {
        listed.extend(column(&tracker.text, "CI checks", |header| {
            header == "Check"
        }));
    }
    catalog
        .named_checks()
        .iter()
        .filter(|check| !listed.contains(check.name.as_str()))
        .map(|check| {
            Finding::new(
                &check.path,
                check.line,
                format!(
                    "`ci: {}` names a check that neither docs/operations/ci.md nor the CI checks in {TRACKER} list (DOC-12)",
                    check.name
                ),
            )
        })
        .collect()
}

/// The contents of the code spans in one column of the tables in `section` of `text`: the
/// column whose header `chooses`.
fn column(text: &str, section: &str, chooses: impl Fn(&str) -> bool) -> Vec<String> {
    let mut spans = Vec::new();
    for table in markdown::tables(text) {
        if table.section.as_deref() != Some(section) {
            continue;
        }
        let Some(index) = table.header.iter().position(|header| chooses(header)) else {
            continue;
        };
        for (_, cells) in &table.rows {
            if let Some(cell) = cells.get(index) {
                spans.extend(cell.split('`').skip(1).step_by(2).map(str::to_owned));
            }
        }
    }
    spans
}

/// Prose outside the inventory holds no unfinished-work markers (DOC-04), and no history markers
/// (DOC-01) outside the documents that are history's homes, the changelog and the ADRs. Code
/// spans and code blocks aren't prose, so a document can name a marker in one.
fn prose_markers(documents: &BTreeMap<&str, markdown::Document>) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (path, document) in documents {
        if path.starts_with(INVENTORY) {
            continue;
        }
        let records_history = *path == CHANGELOG || path.starts_with(ADRS);
        for (line, prose) in &document.prose {
            if !records_history {
                findings.extend(history(path, *line, prose));
            }
            findings.extend(unfinished(path, *line, prose));
        }
    }
    findings
}

/// Comments hold no history markers or dates (DOC-01), and no unfinished-work markers (DOC-04).
fn comment_markers(repo: &Repo) -> Vec<Finding> {
    let mut findings = Vec::new();
    for file in repo.files() {
        for (index, line) in file.text.lines().enumerate() {
            let Some(comment) = text::comment(&file.path, line) else {
                continue;
            };
            findings.extend(history(&file.path, index + 1, comment));
            findings.extend(unfinished(&file.path, index + 1, comment));
            if text::has_date(comment) {
                findings.push(Finding::new(
                    &file.path,
                    index + 1,
                    "a date in a comment records when something was written; describe the current state (DOC-01)",
                ));
            }
        }
    }
    findings
}

/// No line of the API's document holds a history marker (DOC-01).
fn document_markers(repo: &Repo) -> Vec<Finding> {
    repo.files()
        .filter(|file| file.path == DOCUMENT)
        .flat_map(|file| {
            let lines = file.text.lines().enumerate();
            lines
                .flat_map(|(index, line)| history(&file.path, index + 1, line))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn history(path: &str, line: usize, text: &str) -> Vec<Finding> {
    HISTORY
        .iter()
        .filter(|phrase| text::has_phrase(text, phrase))
        .map(|phrase| {
            Finding::new(
                path,
                line,
                format!("\"{phrase}\" describes history; describe the current state (DOC-01)"),
            )
        })
        .collect()
}

fn unfinished(path: &str, line: usize, text: &str) -> Vec<Finding> {
    UNFINISHED
        .iter()
        .filter(|word| text::has_word(text, word))
        .map(|word| {
            Finding::new(
                path,
                line,
                format!("\"{word}\" marks unfinished work; track the work as an issue (DOC-04)"),
            )
        })
        .collect()
}

/// Every Markdown file belongs to the documentation set (DOC-05).
fn locations(repo: &Repo) -> Vec<Finding> {
    repo.files()
        .filter(|file| file.path.ends_with(".md") && !in_documentation_set(&file.path))
        .map(|file| {
            Finding::new(
                &file.path,
                1,
                "this document is outside the documentation set (DOC-05)",
            )
        })
        .collect()
}

/// Whether `path` is a document of the documentation set outside the migration workspace,
/// which DOC-06's check of acronyms and DOC-15's of words read.
pub fn outside_migration(path: &str) -> bool {
    in_documentation_set(path) && !path.starts_with(INVENTORY) && path != TRACKER
}

/// Whether `path` belongs to the documentation set (DOC-05), or to the migration workspace
/// (DOC-13).
fn in_documentation_set(path: &str) -> bool {
    const FILES: [&str; 7] = [
        "README.md",
        "CLAUDE.md",
        "SECURITY.md",
        CHANGELOG,
        "docs/architecture.md",
        "docs/coverage.md",
        TRACKER,
    ];
    const DIRECTORIES: [&str; 7] = [
        "docs/standards/",
        "docs/spec/",
        ADRS,
        "docs/operations/",
        "docs/guides/",
        ".claude/",
        INVENTORY,
    ];
    FILES.contains(&path)
        || DIRECTORIES
            .iter()
            .any(|directory| path.starts_with(directory))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::fixture_with;

    /// The CI document, listing the fixture's one check.
    const CI: &str = "# CI\n\n## Checks that exist\n\n| Check | `just` task | Enforces |\n|---|---|---|\n\
                      | Style | `check-style` | STYLE-01 |\n";

    #[test]
    fn doc_01_the_layout_of_app_names_each_rust_file_directly_in_the_composition_root() {
        let architecture = "```\n  src/\n    composition/   the root, in mod.rs,\n\
                            \x20                  and in tasks.rs\n    modules/       seed.rs\n```\n";
        let repo = Repo::of(&[
            ("docs/architecture.md", architecture),
            ("crates/app/src/composition/mod.rs", ""),
            ("crates/app/src/composition/tasks.rs", ""),
            ("crates/app/src/composition/seed.rs", ""),
            ("crates/app/src/composition/seed/accounts.rs", ""),
        ]);

        let found: Vec<String> = composition(&repo).iter().map(ToString::to_string).collect();

        assert_eq!(
            found,
            [
                "docs/architecture.md:3: the layout of `app` doesn't name composition/seed.rs (DOC-01)"
            ]
        );
    }

    #[test]
    fn doc_01_the_list_of_xtask_s_subcommands_is_the_one_that_check_just_runs() {
        let recipes = "check-a:\n    {{ xtask }} check-a\ncheck-b:\n    {{ xtask }} check-b --all\n\
                       check-s3:\n    {{ xtask }} check-s3\ncheck-at:\n    @{{xtask}} check-at\n\
                       check-in:\n    x=$({{ xtask }} check-in)\nother:\n    cargo run --package app\n";
        // The fixture's marker is built, so that this file holds none.
        let ci = format!(
            "# CI\n\n`check-a`, `check-s3`, `check-at`, `check-in` and `check-c` are subcommands of the `xtask` crate (ARCH-01).\n\
             So are `check-d`. <!-- {} gadgets -->\n\n`check-b` runs elsewhere.\n",
            "part:"
        );
        let repo = Repo::of(&[("just/check.just", recipes), ("docs/operations/ci.md", &ci)]);

        let found: Vec<String> = subcommands(&repo).iter().map(ToString::to_string).collect();

        assert_eq!(
            found,
            [
                "docs/operations/ci.md:3: `check-b`, which just/check.just runs through xtask, isn't in the list of xtask's subcommands (DOC-01)",
                "docs/operations/ci.md:3: the list of xtask's subcommands names `check-c`, which just/check.just doesn't run through xtask (DOC-01)",
                "docs/operations/ci.md:3: the list of xtask's subcommands names `check-d`, which just/check.just doesn't run through xtask (DOC-01)",
            ]
        );
    }

    fn findings(changes: &[(&'static str, &'static str)]) -> Vec<String> {
        let mut all = vec![("docs/operations/ci.md", CI)];
        all.extend_from_slice(changes);
        let repo = Repo::of(&fixture_with(&all));
        check(&repo, &Catalog::read(&repo))
            .findings
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn doc_12_the_fixture_has_no_problems() {
        assert_eq!(findings(&[]), Vec::<String>::new());
    }

    #[test]
    fn doc_12_a_link_to_a_missing_file_or_anchor_is_reported() {
        let readme = "[spec](docs/spec/widgets.md) [ok](docs/architecture.md#what-each-module-owns)\n\
                      [gone](docs/gone.md)\n\
                      [anchor](docs/architecture.md#nowhere)\n\
                      [out](../elsewhere.md) [site](https://example.com) [here](#readme)\n\
                      # Readme\n\nText.\n";

        assert_eq!(
            findings(&[("README.md", readme)]),
            [
                "README.md:2: the link to `docs/gone.md` doesn't resolve: there is no `docs/gone.md` (DOC-12)",
                "README.md:3: the link to `docs/architecture.md#nowhere` doesn't resolve: `docs/architecture.md` has no heading whose anchor is `#nowhere` (DOC-12)",
                "README.md:4: the link to `../elsewhere.md` doesn't resolve: it points outside the repository (DOC-12)"
            ]
        );
    }

    #[test]
    fn doc_12_two_headings_of_one_text_are_reported_linked_or_not() {
        let guide =
            "# Guide\n\n## Notes\n\nText.\n\n## Steps\nSteps.\n## Notes\n\n```\n## Notes\n```\n";
        let inventory = "# Old\n\n## Notes\n\n## Notes\n";

        assert_eq!(
            findings(&[
                ("docs/guides/guide.md", guide),
                ("docs/inventory/old.md", inventory)
            ]),
            [
                "docs/guides/guide.md:9: an earlier heading also reads `Notes`, so their anchors \
                 follow their order: give each heading a text of its own (DOC-12)"
            ]
        );
    }

    #[test]
    fn doc_12_the_changelog_repeats_its_sections_headings_and_no_link_names_one() {
        let changelog = "# Changelog\n\n## [Unreleased]\n\n## [1.1.0]\n\n### Added\n\n- A.\n\n\
                         ## [1.0.0]\n\n### Added\n\n- B.\n";
        let readme = "# Readme\n\n[added](CHANGELOG.md#added) [release](CHANGELOG.md#100)\n";

        assert_eq!(
            findings(&[(CHANGELOG, changelog), ("README.md", readme)]),
            [
                "README.md:3: the link to `CHANGELOG.md#added` doesn't resolve: another heading \
                 of `CHANGELOG.md` has the text of the one `#added` names, so the anchor follows \
                 their order: give each heading a text of its own (DOC-12)"
            ]
        );
    }

    #[test]
    fn doc_12_an_id_that_doesnt_exist_is_reported() {
        let source = "/// Proves WIDGETS-001 and STYLE-01, but not WIDGETS-007, GADGETS-001 or STYLE-99.\n\
                      /// SHA-256 isn't an ID.\n";

        assert_eq!(
            findings(&[("crates/widgets/src/lib.rs", source)]),
            [
                "crates/widgets/src/lib.rs:1: WIDGETS-007 isn't a rule or behavior ID that exists (DOC-12)",
                "crates/widgets/src/lib.rs:1: GADGETS-001 isn't a rule or behavior ID that exists (DOC-12)",
                "crates/widgets/src/lib.rs:1: STYLE-99 isn't a rule or behavior ID that exists (DOC-12)"
            ]
        );
    }

    #[test]
    fn doc_12_a_doc_comment_s_link_that_doesn_t_resolve_is_reported() {
        let source = "//! The module ([spec](../../docs/spec/widgets.md)).\n\
                      /// See [the gone spec](../../docs/spec/gadgets.md), [`Item`], \
                      [cfg](super::cfg) and `[text](destination)`.\n\
                      // A plain comment's [link](nowhere.md) isn't documentation.\n";

        assert_eq!(
            findings(&[("crates/widgets/lib.rs", source)]),
            [
                "crates/widgets/lib.rs:2: the link to `../../docs/spec/gadgets.md` doesn't resolve: there is no `docs/spec/gadgets.md` (DOC-12)"
            ]
        );
    }

    #[test]
    fn doc_12_a_check_s_tests_cite_ids_of_their_own() {
        let source = "/// Reads WIDGETS-007.\nfn check() {}\n\n#[cfg(test)]\nmod tests {\n    \
                      // A made-up module's WIDGETS-009, and `widgets_009_` its test prefix.\n}\n";

        assert_eq!(
            findings(&[("crates/xtask/src/widgets.rs", source)]),
            [
                "crates/xtask/src/widgets.rs:1: WIDGETS-007 isn't a rule or behavior ID that exists (DOC-12)"
            ]
        );
    }

    #[test]
    fn doc_12_a_named_check_that_no_list_includes_is_reported() {
        let rules = "# Style\n\n### STYLE-01 A rule\nThe statement.\n- Why: a reason.\n\
                     - Check: `ci: check-shape`, `ci: check-style` and `ci: check-later`.\n";
        let ci = "# CI\n\nThe prose names `check-shape`, which lists nothing.\n\n\
                  ## Checks that exist\n\n| Check | `just` task | Enforces |\n|---|---|---|\n\
                  | Style | `check-style` | STYLE-01 |\n";
        let tracker = "# Tracker\n\n## Current work\n\nNext, `check-shape`.\n\n## Decisions\n\n\
                       ## CI checks\n\n| Check | Step |\n|---|---|\n| `check-later` | 2 |\n";

        let found = findings(&[
            ("docs/standards/style.md", rules),
            ("docs/operations/ci.md", ci),
            (TRACKER, tracker),
        ]);

        assert!(
            found.contains(&"docs/standards/style.md:6: `ci: check-shape` names a check that neither docs/operations/ci.md nor the CI checks in docs/MIGRATION.md list (DOC-12)".to_owned()),
            "{found:?}"
        );
        assert!(
            !found
                .iter()
                .any(|finding| finding.contains("check-style") || finding.contains("check-later")),
            "{found:?}"
        );
    }

    #[test]
    fn doc_01_history_markers_in_prose_and_comments_are_reported() {
        let spec = "# Widgets\n\nIt previously used `legacy` names.\n\n```\nused to\n```\n";
        let tracker = "# Tracker\n\n## Current work\n\nThis no longer applies.\n\n## Decisions\n";
        let config = "name = \"a\" # set on 2026-01-02\n";

        assert_eq!(
            findings(&[
                ("docs/guides/history.md", spec),
                (TRACKER, tracker),
                ("Cargo.toml", config),
            ]),
            [
                "docs/MIGRATION.md:5: \"no longer\" describes history; describe the current state (DOC-01)",
                "docs/guides/history.md:3: \"previously\" describes history; describe the current state (DOC-01)",
                "Cargo.toml:1: a date in a comment records when something was written; describe the current state (DOC-01)"
            ]
        );
    }

    #[test]
    fn doc_01_history_markers_on_a_line_of_the_api_s_document_are_reported() {
        let document =
            "{\n  \"description\": \"The session, whose account No longer needs it.\"\n}\n";

        let marked: Vec<String> = findings(&[(DOCUMENT, document)])
            .into_iter()
            .filter(|finding| finding.ends_with("(DOC-01)"))
            .collect();

        assert_eq!(
            marked,
            [
                "openapi.json:2: \"no longer\" describes history; describe the current state (DOC-01)"
            ]
        );
    }

    #[test]
    fn doc_01_the_changelog_and_an_adr_say_what_changed_and_no_other_document_does() {
        let notes = "# Changelog\n\nA task no longer takes its lock, as it previously did.\n";
        let marked: Vec<String> = findings(&[
            (CHANGELOG, notes),
            ("docs/adr/0001-locks.md", notes),
            ("docs/guides/locks.md", notes),
        ])
        .into_iter()
        .filter(|finding| finding.ends_with("(DOC-01)"))
        .collect();

        assert_eq!(
            marked,
            [
                "docs/guides/locks.md:3: \"previously\" describes history; describe the current state (DOC-01)",
                "docs/guides/locks.md:3: \"no longer\" describes history; describe the current state (DOC-01)",
            ]
        );
    }

    #[test]
    fn doc_04_unfinished_work_markers_are_reported() {
        // The markers are split, so this file doesn't contain them.
        let prose = concat!("# Notes\n\nFinish this ", "TO", "DO.\n");
        let script = concat!("run: ok # ", "FIX", "ME later\n");

        assert_eq!(
            findings(&[("docs/guides/notes.md", prose), ("ci.yml", script)]),
            [
                concat!(
                    "docs/guides/notes.md:3: \"",
                    "TO",
                    "DO\" marks unfinished work; track the work as an issue (DOC-04)"
                ),
                concat!(
                    "ci.yml:1: \"",
                    "FIX",
                    "ME\" marks unfinished work; track the work as an issue (DOC-04)"
                )
            ]
        );
    }

    #[test]
    fn doc_05_a_document_outside_the_documentation_set_is_reported() {
        assert_eq!(
            findings(&[("crates/widgets/NOTES.md", "# Notes\n")]),
            ["crates/widgets/NOTES.md:1: this document is outside the documentation set (DOC-05)"]
        );
    }
}
