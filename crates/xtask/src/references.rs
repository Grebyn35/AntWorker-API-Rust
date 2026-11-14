//! `check-docs`, for what documents and comments name besides links and IDs:
//! - a behavior's or rule's test prefix, such as `workspaces_005`, names an ID that exists
//!   (DOC-12);
//! - a path to a Rust file, such as `events/hub.rs`, names a file that exists (DOC-01);
//! - no document or comment names a codebase this one replaces, by its name or by a phrase, and
//!   only the places DOC-13 lists point into the inventory (DOC-13).
//!
//! It reads every Markdown document, and every comment of a file whose comments `text` can find,
//! but the inventory's, whose IDs are its own, for test prefixes and Rust paths; and every text
//! file but the migration workspace's, whose tracker and inventory record the codebase this one
//! replaces, for its name, the phrases that name it and the inventory's paths, outside a comment
//! as well as in one.

use crate::{Finding, catalog::Catalog, repo::Repo, text};

/// The inventory, whose IDs are its own (DOC-12).
const INVENTORY: &str = "docs/inventory/";

/// The migration workspace (DOC-13).
const WORKSPACE: [&str; 2] = [INVENTORY, "docs/MIGRATION.md"];

/// The places besides the workspace that may point into the inventory (DOC-13).
const INVENTORY_POINTERS: [&str; 4] = [
    "CLAUDE.md",
    ".claude/skills/conformance-review/SKILL.md",
    "docs/architecture.md",
    "docs/standards/docs.md",
];

/// The names of the codebases this one replaces (DOC-13).
const OLD_NAMES: [&str; 1] = ["AntWorker-API"];

/// The phrases that name the codebases this one replaces without their names (DOC-13). CLAUDE.md,
/// a pointer into the inventory, may say what the inventory describes.
const OLD_PHRASES: [&str; 3] = ["the old API", "the old client", "the old codebase"];

/// Every test prefix, Rust path, old name and inventory path that the documents and comments name
/// wrongly.
pub fn check(repo: &Repo, catalog: &Catalog) -> Vec<Finding> {
    let rust_files: Vec<&str> = repo.paths().filter(|path| path.ends_with(".rs")).collect();
    let mut findings = Vec::new();
    for file in repo.files() {
        let path = file.path.as_str();
        if path.starts_with(INVENTORY) {
            continue;
        }
        let workspace = WORKSPACE.iter().any(|place| path.starts_with(place));
        let markdown = path.ends_with(".md");
        let rust = path.ends_with(".rs");
        let citing = text::citing_lines(path, &file.text);
        for (index, line) in file.text.lines().enumerate() {
            let mut found = |message: String| findings.push(Finding::new(path, index + 1, message));
            // A Rust file's code names what it checks, such as the inventory's path; its comments
            // describe it, as every other file's comments do.
            let comment = text::comment(path, line);
            let described = if markdown { Some(line) } else { comment };
            let cites = index < citing;
            for (prefix, number) in described
                .filter(|_| cites)
                .map(test_prefixes)
                .unwrap_or_default()
            {
                let id = format!("{}-{number}", prefix.to_ascii_uppercase());
                if catalog.knows_prefix(&prefix.to_ascii_uppercase()) && !catalog.defines(&id) {
                    found(format!(
                        "`{prefix}_{number}` is the test prefix of {id}, which doesn't exist (DOC-12)"
                    ));
                }
            }
            for cited in described.map(rust_paths).unwrap_or_default() {
                let exists = rust_files
                    .iter()
                    .any(|file| *file == cited || file.ends_with(&format!("/{cited}")));
                if !exists {
                    found(format!(
                        "`{cited}` names no file of the repository (DOC-01)"
                    ));
                }
            }
            if workspace {
                continue;
            }
            let Some(text) = (if rust { comment } else { Some(line) }) else {
                continue;
            };
            for name in OLD_NAMES {
                let named = text.match_indices(name).any(|(start, _)| {
                    !text[start + name.len()..]
                        .starts_with(|c: char| c.is_alphanumeric() || c == '-')
                });
                if named {
                    found(format!(
                        "`{name}` names the old codebase, which a fork never saw (DOC-13)"
                    ));
                }
            }
            for phrase in OLD_PHRASES {
                if text::has_phrase(text, phrase) && !INVENTORY_POINTERS.contains(&path) {
                    found(format!(
                        "\"{phrase}\" names the old codebase, which a fork never saw (DOC-13)"
                    ));
                }
            }
            if text.contains("inventory/") && !INVENTORY_POINTERS.contains(&path) {
                found(
                    "a path into the inventory, which only the places DOC-13 lists hold (DOC-13)"
                        .to_owned(),
                );
            }
        }
    }
    findings
}

/// The test prefixes in `text`: a word of lowercase letters and underscores, then `_` and two or
/// three digits, as `workspaces_005` or `data_19`, followed by `_` or no letter or digit; each
/// as its module or rule prefix and its number.
fn test_prefixes(text: &str) -> Vec<(String, String)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let word_start =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        if !(word_start && bytes[start].is_ascii_lowercase()) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < bytes.len() && (bytes[end].is_ascii_lowercase() || bytes[end] == b'_') {
            end += 1;
        }
        let digits = bytes[end..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let after = bytes.get(end + digits).copied();
        let ends = after.is_none_or(|byte| byte == b'_' || !byte.is_ascii_alphanumeric());
        if end > start + 1 && bytes[end - 1] == b'_' && (2..=3).contains(&digits) && ends {
            found.push((
                text[start..end - 1].to_owned(),
                text[end..end + digits].to_owned(),
            ));
        }
        start = end.max(start + 1);
    }
    found
}

/// The paths to Rust files in `text` that name a directory, such as `events/hub.rs`: a bare file
/// name, such as `jobs.rs`, names the file of every module.
fn rust_paths(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (end, _) in text.match_indices(".rs") {
        let after = text[end + 3..].chars().next();
        if after.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let start = text[..end]
            .rfind(|c: char| {
                !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '/')
            })
            .map_or(0, |at| at + 1);
        let path = &text[start..end + 3];
        let before = text[..start].chars().next_back();
        let named = path.contains('/')
            && !path.starts_with('/')
            && !path.contains("//")
            && !before.is_some_and(|c| c == '.' || c == '-' || c == ':' || c.is_alphanumeric());
        if named {
            found.push(path.to_owned());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_12_a_test_prefix_is_read_with_its_number() {
        assert_eq!(
            test_prefixes("see `workspaces_001_` and data_19, not sha_256x or a_1"),
            [
                ("workspaces".to_owned(), "001".to_owned()),
                ("data".to_owned(), "19".to_owned()),
            ]
        );
        assert_eq!(
            test_prefixes("client_telemetry_004_reports"),
            [("client_telemetry".to_owned(), "004".to_owned())]
        );
    }

    #[test]
    fn doc_01_a_rust_path_names_a_directory() {
        assert_eq!(
            rust_paths("in events/hub.rs, and `system/seed.rs`; not jobs.rs, ../a/b.rs or x.rsx"),
            ["events/hub.rs", "system/seed.rs"]
        );
    }

    #[test]
    fn doc_13_and_doc_13_documents_and_comments_name_what_exists() {
        let repo = Repo::of(&[
            ("docs/spec/things.md", "| ID |\n|---|\n| THINGS-001 |\n"),
            ("crates/platform/src/events/hub.rs", "// The hub.\n"),
            (
                "docs/guide.md",
                "Proven by `things_001_` and `things_002_`, in events/hub.rs and events/gone.rs.\n\
                 The old AntWorker-API did it; AntWorker-API-Rust does it.\n\
                 See [the trace](inventory/decision-trace.md).\nThe old API never did.\n",
            ),
            (
                "crates/app/src/lib.rs",
                "let path = \"docs/inventory/\";\n// things_009_ proves it.\n",
            ),
            (
                "docs/MIGRATION.md",
                "AntWorker-API, things_005_, gone/old.rs, inventory/x.md, things_001_\n",
            ),
            ("docs/inventory/old.md", "things_007_ in old/file.rs\n"),
            (
                "deploy/tasks/old.sh",
                "# As AntWorker-API did.\n. ./docs/inventory/x.sh\n",
            ),
            (
                "justfile",
                "# things_008_ is read here, as every comment is.\n",
            ),
        ]);
        let catalog = Catalog::read(&repo);

        let messages: Vec<String> = check(&repo, &catalog)
            .into_iter()
            .map(|finding| format!("{}:{}", finding.path, finding.message))
            .collect();

        assert_eq!(messages.len(), 11, "{messages:#?}");
        for expected in [
            "docs/guide.md:`things_002` is the test prefix of THINGS-002",
            "docs/guide.md:`events/gone.rs` names no file",
            "docs/guide.md:`AntWorker-API` names the old codebase",
            "docs/guide.md:\"the old API\" names the old codebase",
            "docs/guide.md:a path into the inventory",
            "crates/app/src/lib.rs:`things_009` is the test prefix",
            "docs/MIGRATION.md:`things_005` is the test prefix",
            "docs/MIGRATION.md:`gone/old.rs` names no file",
            "deploy/tasks/old.sh:`AntWorker-API` names the old codebase",
            "deploy/tasks/old.sh:a path into the inventory",
            "justfile:`things_008` is the test prefix",
        ] {
            assert!(
                messages.iter().any(|message| message.starts_with(expected)),
                "{expected} in {messages:#?}"
            );
        }
    }
}
