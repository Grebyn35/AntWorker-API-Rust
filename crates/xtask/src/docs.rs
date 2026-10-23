//! `check-docs`: links, IDs and the CI checks the rules name all resolve (DOC-13); prose and
//! comments hold no history markers (DOC-01) and no unfinished-work markers (DOC-07); and every
//! document belongs to the documentation set (DOC-08).

use std::collections::{BTreeMap, BTreeSet};

use crate::{Finding, Outcome, catalog::Catalog, formats, markdown, repo::Repo, text};

/// Words that describe history instead of the current state (DOC-01).
const HISTORY: [&str; 5] = ["previously", "no longer", "used to", "legacy", "now uses"];

/// Words that mark unfinished work (DOC-07).
const UNFINISHED: [&str; 3] = ["TODO", "FIXME", "XXX"];

/// The migration workspace's inventory (DOC-04), which numbers its entries with IDs of its own.
/// No ID, marker or comment in it is checked.
const INVENTORY: &str = "docs/inventory/";

/// The migration tracker, the other part of the migration workspace (DOC-04).
const TRACKER: &str = "docs/MIGRATION.md";

/// Runs the check.
pub fn check(repo: &Repo, catalog: &Catalog) -> Outcome {
    let documents: BTreeMap<&str, markdown::Document> = repo
        .files()
        .filter(|file| file.path.ends_with(".md"))
        .map(|file| (file.path.as_str(), markdown::read(&file.text)))
        .collect();
    let mut findings = catalog.findings().to_vec();
    findings.extend(links(repo, &documents));
    findings.extend(ids(repo, catalog));
    findings.extend(named_checks(repo, catalog));
    findings.extend(prose_markers(&documents));
    findings.extend(comment_markers(repo));
    findings.extend(locations(repo));
    findings.extend(formats::check(repo));
    Outcome {
        report: vec![format!(
            "check-docs: {} files checked, {} of them documents.",
            repo.files().count(),
            documents.len()
        )],
        findings,
    }
}

/// Every link to a file of the repository resolves, and so does its anchor (DOC-13).
fn links(repo: &Repo, documents: &BTreeMap<&str, markdown::Document>) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (path, document) in documents {
        for link in &document.links {
            if let Some(problem) = unresolved(repo, documents, path, &link.destination) {
                findings.push(Finding::new(
                    path,
                    link.line,
                    format!(
                        "the link to `{}` doesn't resolve: {problem} (DOC-13)",
                        link.destination
                    ),
                ));
            }
        }
    }
    findings
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
    let (target, anchor) = destination.split_once('#').unwrap_or((destination, ""));
    let target = if target.is_empty() {
        from.to_owned()
    } else {
        let parent = from.rsplit_once('/').map_or("", |(directory, _)| directory);
        let Some(joined) = join(parent, target) else {
            return Some("it points outside the repository".to_owned());
        };
        joined
    };
    if !repo.has(&target) {
        return Some(format!("there is no `{target}`"));
    }
    match documents.get(target.as_str()) {
        Some(document) if !anchor.is_empty() && !document.anchors.contains(anchor) => Some(
            format!("`{target}` has no heading whose anchor is `#{anchor}`"),
        ),
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

/// Every rule or behavior ID cited outside the inventory exists (DOC-13). A token whose prefix
/// no rule or module uses, such as `SHA-256`, isn't an ID.
fn ids(repo: &Repo, catalog: &Catalog) -> Vec<Finding> {
    let mut findings = Vec::new();
    let cited = repo.files().filter(|file| {
        !file.path.starts_with(INVENTORY)
            && !file.path.starts_with(".sqlx/")
            && file.path != "Cargo.lock"
    });
    for file in cited {
        for (index, line) in file.text.lines().enumerate() {
            for token in text::id_tokens(line) {
                let id = token.id();
                if catalog.knows_prefix(token.prefix) && !catalog.defines(&id) {
                    findings.push(Finding::new(
                        &file.path,
                        index + 1,
                        format!("{id} isn't a rule or behavior ID that exists (DOC-13)"),
                    ));
                }
            }
        }
    }
    findings
}

/// Every CI check a rule names is listed: in the task column of the table of checks in
/// `docs/operations/ci.md` once it is built, and in the first column of the tracker's table of
/// CI checks until then (DOC-13).
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
                    "`ci: {}` names a check that neither docs/operations/ci.md nor the CI checks in {TRACKER} list (DOC-13)",
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

/// Prose outside the inventory holds no history markers (DOC-01) and no unfinished-work markers
/// (DOC-07). Code spans and code blocks aren't prose, so a document can name a marker in one.
fn prose_markers(documents: &BTreeMap<&str, markdown::Document>) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (path, document) in documents {
        if path.starts_with(INVENTORY) {
            continue;
        }
        for (line, prose) in &document.prose {
            findings.extend(history(path, *line, prose));
            findings.extend(unfinished(path, *line, prose));
        }
    }
    findings
}

/// Comments hold no history markers or dates (DOC-01), and no unfinished-work markers (DOC-07).
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
                format!("\"{word}\" marks unfinished work; track the work as an issue (DOC-07)"),
            )
        })
        .collect()
}

/// Every Markdown file belongs to the documentation set (DOC-08).
fn locations(repo: &Repo) -> Vec<Finding> {
    repo.files()
        .filter(|file| file.path.ends_with(".md") && !in_documentation_set(&file.path))
        .map(|file| {
            Finding::new(
                &file.path,
                1,
                "this document is outside the documentation set (DOC-08)",
            )
        })
        .collect()
}

/// Whether `path` belongs to the documentation set (DOC-08), or to the migration workspace
/// (DOC-04).
fn in_documentation_set(path: &str) -> bool {
    const FILES: [&str; 7] = [
        "README.md",
        "CLAUDE.md",
        "SECURITY.md",
        "CHANGELOG.md",
        "docs/architecture.md",
        "docs/coverage.md",
        TRACKER,
    ];
    const DIRECTORIES: [&str; 7] = [
        "docs/standards/",
        "docs/spec/",
        "docs/adr/",
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
    fn doc_13_the_fixture_has_no_problems() {
        assert_eq!(findings(&[]), Vec::<String>::new());
    }

    #[test]
    fn doc_13_a_link_to_a_missing_file_or_anchor_is_reported() {
        let readme = "[spec](docs/spec/widgets.md) [ok](docs/architecture.md#what-each-module-owns)\n\
                      [gone](docs/gone.md)\n\
                      [anchor](docs/architecture.md#nowhere)\n\
                      [out](../elsewhere.md) [site](https://example.com) [here](#readme)\n\
                      # Readme\n";

        assert_eq!(
            findings(&[("README.md", readme)]),
            [
                "README.md:2: the link to `docs/gone.md` doesn't resolve: there is no `docs/gone.md` (DOC-13)",
                "README.md:3: the link to `docs/architecture.md#nowhere` doesn't resolve: `docs/architecture.md` has no heading whose anchor is `#nowhere` (DOC-13)",
                "README.md:4: the link to `../elsewhere.md` doesn't resolve: it points outside the repository (DOC-13)"
            ]
        );
    }

    #[test]
    fn doc_13_an_id_that_doesnt_exist_is_reported() {
        let source = "/// Proves WIDGETS-001 and STYLE-01, but not WIDGETS-007, GADGETS-001 or STYLE-99.\n\
                      /// SHA-256 isn't an ID.\n";

        assert_eq!(
            findings(&[("crates/widgets/src/lib.rs", source)]),
            [
                "crates/widgets/src/lib.rs:1: WIDGETS-007 isn't a rule or behavior ID that exists (DOC-13)",
                "crates/widgets/src/lib.rs:1: GADGETS-001 isn't a rule or behavior ID that exists (DOC-13)",
                "crates/widgets/src/lib.rs:1: STYLE-99 isn't a rule or behavior ID that exists (DOC-13)"
            ]
        );
    }

    #[test]
    fn doc_13_a_named_check_that_no_list_includes_is_reported() {
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
            found.contains(&"docs/standards/style.md:6: `ci: check-shape` names a check that neither docs/operations/ci.md nor the CI checks in docs/MIGRATION.md list (DOC-13)".to_owned()),
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
    fn doc_07_unfinished_work_markers_are_reported() {
        // The markers are split, so this file doesn't contain them.
        let prose = concat!("# Notes\n\nFinish this ", "TO", "DO.\n");
        let script = concat!("run: ok # ", "FIX", "ME later\n");

        assert_eq!(
            findings(&[("docs/guides/notes.md", prose), ("ci.yml", script)]),
            [
                concat!(
                    "docs/guides/notes.md:3: \"",
                    "TO",
                    "DO\" marks unfinished work; track the work as an issue (DOC-07)"
                ),
                concat!(
                    "ci.yml:1: \"",
                    "FIX",
                    "ME\" marks unfinished work; track the work as an issue (DOC-07)"
                )
            ]
        );
    }

    #[test]
    fn doc_08_a_document_outside_the_documentation_set_is_reported() {
        assert_eq!(
            findings(&[("crates/widgets/NOTES.md", "# Notes\n")]),
            ["crates/widgets/NOTES.md:1: this document is outside the documentation set (DOC-08)"]
        );
    }
}
