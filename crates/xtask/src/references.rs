//! `check-docs`, for what documents and comments name besides links and IDs:
//! - a behavior's or rule's test prefix, such as `workspaces_001`, names an ID that exists
//!   (DOC-13);
//! - a path to a Rust file, such as `events/hub.rs`, names a file that exists (DOC-01);
//! - no document or comment names the old codebase, and only the places DOC-04 lists point into
//!   the inventory (DOC-04).
//!
//! It reads every Markdown document and every comment of a Rust file, outside the migration
//! workspace, whose tracker and inventory record the old codebase and the code as it was.

use crate::{Finding, catalog::Catalog, repo::Repo};

/// The migration workspace (DOC-04).
const WORKSPACE: [&str; 2] = ["docs/inventory/", "docs/MIGRATION.md"];

/// The places besides the workspace that may point into the inventory (DOC-04).
const INVENTORY_POINTERS: [&str; 4] = [
    "CLAUDE.md",
    ".claude/skills/conformance-review/SKILL.md",
    "docs/architecture.md",
    "docs/standards/docs.md",
];

/// The names of the codebases this one replaces (DOC-04).
const OLD_NAMES: [&str; 1] = ["AntWorker-API"];

/// Every test prefix, Rust path, old name and inventory path that the documents and comments name
/// wrongly.
pub fn check(repo: &Repo, catalog: &Catalog) -> Vec<Finding> {
    let rust_files: Vec<&str> = repo.paths().filter(|path| path.ends_with(".rs")).collect();
    let mut findings = Vec::new();
    for file in repo.files() {
        let path = file.path.as_str();
        if WORKSPACE.iter().any(|place| path.starts_with(place)) {
            continue;
        }
        let markdown = path.ends_with(".md");
        if !markdown && !path.ends_with(".rs") {
            continue;
        }
        for (index, line) in file.text.lines().enumerate() {
            let text = if markdown {
                line
            } else {
                match line.find("//") {
                    Some(start) => &line[start..],
                    None => continue,
                }
            };
            let mut found = |message: String| findings.push(Finding::new(path, index + 1, message));
            for (prefix, number) in test_prefixes(text) {
                let id = format!("{}-{number}", prefix.to_ascii_uppercase());
                if catalog.knows_prefix(&prefix.to_ascii_uppercase()) && !catalog.defines(&id) {
                    found(format!(
                        "`{prefix}_{number}` is the test prefix of {id}, which doesn't exist (DOC-13)"
                    ));
                }
            }
            for cited in rust_paths(text) {
                let exists = rust_files
                    .iter()
                    .any(|file| *file == cited || file.ends_with(&format!("/{cited}")));
                if !exists {
                    found(format!(
                        "`{cited}` names no file of the repository (DOC-01)"
                    ));
                }
            }
            for name in OLD_NAMES {
                let named = text.match_indices(name).any(|(start, _)| {
                    !text[start + name.len()..]
                        .starts_with(|c: char| c.is_alphanumeric() || c == '-')
                });
                if named {
                    found(format!(
                        "`{name}` names the old codebase, which a fork never saw (DOC-04)"
                    ));
                }
            }
            if text.contains("inventory/") && !INVENTORY_POINTERS.contains(&path) {
                found(
                    "a path into the inventory, which only the places DOC-04 lists hold (DOC-04)"
                        .to_owned(),
                );
            }
        }
    }
    findings
}

/// The test prefixes in `text`: a word of lowercase letters and underscores, then `_` and two or
/// three digits, as `workspaces_001` or `data_19`, followed by `_` or no letter or digit; each
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
    fn doc_13_a_test_prefix_is_read_with_its_number() {
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
    fn doc_04_and_doc_13_documents_and_comments_name_what_exists() {
        let repo = Repo::of(&[
            ("docs/spec/things.md", "| ID |\n|---|\n| THINGS-001 |\n"),
            ("crates/platform/src/events/hub.rs", "// The hub.\n"),
            (
                "docs/guide.md",
                "Proven by `things_001_` and `things_002_`, in events/hub.rs and events/gone.rs.\n\
                 The old AntWorker-API did it; AntWorker-API-Rust does it.\n\
                 See [the trace](inventory/decision-trace.md).\n",
            ),
            (
                "crates/app/src/lib.rs",
                "let path = \"docs/inventory/\";\n// things_009_ proves it.\n",
            ),
            (
                "docs/MIGRATION.md",
                "AntWorker-API, things_005_, gone/old.rs, inventory/x.md\n",
            ),
        ]);
        let catalog = Catalog::read(&repo);

        let messages: Vec<String> = check(&repo, &catalog)
            .into_iter()
            .map(|finding| format!("{}:{}", finding.path, finding.message))
            .collect();

        assert_eq!(messages.len(), 5, "{messages:#?}");
        for expected in [
            "docs/guide.md:`things_002` is the test prefix of THINGS-002",
            "docs/guide.md:`events/gone.rs` names no file",
            "docs/guide.md:`AntWorker-API` names the old codebase",
            "docs/guide.md:a path into the inventory",
            "crates/app/src/lib.rs:`things_009` is the test prefix",
        ] {
            assert!(
                messages.iter().any(|message| message.starts_with(expected)),
                "{expected} in {messages:#?}"
            );
        }
    }
}
