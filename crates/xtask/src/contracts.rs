//! `check-docs`, the personal data contracts: the accounts spec's two tables of them, what a
//! deletion erases and what an export holds, each hold a row for every contract that the
//! composition root registers, and none for another (DOC-07).
//!
//! The check reads the contracts as text: the name before each `::Contract` that follows
//! `Arc::new(` on a line between the line that starts `fn contracts(` in
//! `crates/app/src/composition/mod.rs` and the next line that is `}` alone, where
//! `personal_data` is `platform`'s. A table is the first below its heading, and a row names its
//! contract with the first text between backticks of its first cell. A repository that lacks
//! the composition root's file or the accounts spec isn't read.

use std::collections::BTreeSet;

use crate::{Finding, repo::Repo};

/// The composition root, which registers the contracts.
const COMPOSITION: &str = "crates/app/src/composition/mod.rs";

/// The accounts spec, which holds the tables.
const SPEC: &str = "docs/spec/accounts.md";

/// The headings of the two tables.
const TABLES: [&str; 2] = ["### Deleting an account", "### What an export holds"];

/// A finding for each registered contract that one of the two tables has no row for, for each
/// row of one whose contract the composition root doesn't register, and for what the check
/// can't read.
pub fn check(repo: &Repo) -> Vec<Finding> {
    let (Some(composition), Some(spec)) = (repo.file(COMPOSITION), repo.file(SPEC)) else {
        return Vec::new();
    };
    let Some(registered) = registered(&composition.text) else {
        return vec![Finding::new(
            COMPOSITION,
            1,
            "check-docs can't read the personal data contracts: no line starts `fn \
             contracts(` before a line that is `}` alone (DOC-07)",
        )];
    };
    let lines: Vec<&str> = spec.text.lines().collect();
    let mut findings = Vec::new();
    for heading in TABLES {
        let Some(rows) = rows(&lines, heading) else {
            findings.push(Finding::new(
                SPEC,
                1,
                format!("check-docs finds no table of the contracts below `{heading}` (DOC-07)"),
            ));
            continue;
        };
        let named: BTreeSet<&str> = rows.iter().map(|(_, name)| *name).collect();
        for (line, name) in &rows {
            if !registered.contains(*name) {
                findings.push(Finding::new(
                    SPEC,
                    *line,
                    format!(
                        "this row is of the contract of `{name}`, which `contracts()` in \
                         {COMPOSITION} doesn't register (DOC-07)"
                    ),
                ));
            }
        }
        let table = rows.first().map_or(1, |(line, _)| *line);
        for name in registered.iter().filter(|name| !named.contains(*name)) {
            findings.push(Finding::new(
                SPEC,
                table,
                format!(
                    "the table below `{heading}` has no row for the contract of `{name}`, \
                     which `contracts()` in {COMPOSITION} registers (DOC-07)"
                ),
            ));
        }
    }
    findings
}

/// The modules whose contracts `composition`, the text of the composition root's file,
/// registers, or `None` where it holds no `fn contracts(` that a `}` alone on its line ends.
fn registered(composition: &str) -> Option<BTreeSet<&str>> {
    let mut lines = composition
        .lines()
        .skip_while(|line| !line.starts_with("fn contracts("));
    lines.next()?;
    let mut names = BTreeSet::new();
    for line in lines {
        if line == "}" {
            return Some(names);
        }
        let Some((_, after)) = line.split_once("Arc::new(") else {
            continue;
        };
        if let Some((name, _)) = after.split_once("::Contract") {
            names.insert(if name == "personal_data" {
                "platform"
            } else {
                name
            });
        }
    }
    None
}

/// The rows of the first table below `heading` among `lines`, after its header and its
/// delimiter, each as its line and the contract it names, or `None` where no table stands
/// there before the next heading.
fn rows<'a>(lines: &[&'a str], heading: &str) -> Option<Vec<(usize, &'a str)>> {
    let at = lines.iter().position(|line| *line == heading)?;
    let table = lines[at + 1..]
        .iter()
        .position(|line| line.starts_with('|') || line.starts_with('#'))
        .map(|offset| at + 1 + offset)
        .filter(|table| lines[*table].starts_with('|'))?;
    Some(
        lines[table..]
            .iter()
            .enumerate()
            .take_while(|(_, line)| line.starts_with('|'))
            .skip(2)
            .filter_map(|(offset, line)| {
                let cell = line.trim_start_matches('|').split('|').next()?;
                let name = cell.split('`').nth(1)?;
                Some((table + offset + 1, name))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOSED: &str = "fn routes() {\n}\n\nfn contracts(mail: &Mail) -> Vec<Arc<dyn PersonalData>> {\n    vec![\n        Arc::new(access::Contract),\n        // part: gadgets\n        Arc::new(gadgets::Contract),\n        // end part: gadgets\n        Arc::new(personal_data::Contract),\n        Arc::new(accounts::Contract::new(mail.clone())),\n    ]\n}\n\nfn other() {\n    Arc::new(unread::Contract);\n}\n";

    /// What the check finds with the accounts spec `spec`, each as its line and message.
    fn found(spec: &'static str) -> Vec<String> {
        let repo = Repo::of(&[(COMPOSITION, COMPOSED), (SPEC, spec)]);
        check(&repo)
            .iter()
            .map(|finding| format!("{}: {}", finding.line, finding.message))
            .collect()
    }

    #[test]
    fn doc_07_each_table_of_the_contracts_holds_a_row_for_each_that_is_registered() {
        let complete = "### Deleting an account\n\nSteps.\n\n| Contract | What it erases |\n|---|---|\n\
                        | `access` | a |\n| `gadgets` | b |\n| `platform` | c |\n| `accounts` | d |\n\n\
                        ### What an export holds\n\n| Contract | What it exports |\n|---|---|\n\
                        | `access` | a |\n| `gadgets` | b |\n| `gadgets`, for more | b |\n\
                        | `platform` | c |\n| `accounts` | d |\n";
        let lacking = "### Deleting an account\n\n| Contract | What it erases |\n|---|---|\n\
                       | `access` | a |\n| `widgets` | b |\n| `platform` | c |\n| `accounts` | d |\n\n\
                       ### What an export holds\n\nNo table.\n\n### Later\n\n| `access` | a |\n";

        assert_eq!(found(complete), Vec::<String>::new());
        assert_eq!(
            found(lacking),
            [
                "6: this row is of the contract of `widgets`, which `contracts()` in \
                 crates/app/src/composition/mod.rs doesn't register (DOC-07)",
                "5: the table below `### Deleting an account` has no row for the contract of \
                 `gadgets`, which `contracts()` in crates/app/src/composition/mod.rs registers \
                 (DOC-07)",
                "1: check-docs finds no table of the contracts below `### What an export \
                 holds` (DOC-07)",
            ]
        );
    }

    #[test]
    fn doc_07_a_composition_root_without_its_contracts_is_a_finding_and_a_repository_without_the_files_is_not_read()
     {
        let repo = Repo::of(&[(COMPOSITION, "fn compose() {}\n"), (SPEC, "")]);
        assert_eq!(check(&repo).len(), 1);
        assert!(check(&Repo::of(&[(SPEC, "")])).is_empty());
        assert!(check(&Repo::of(&[(COMPOSITION, COMPOSED)])).is_empty());
    }
}
