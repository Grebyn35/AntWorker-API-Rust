//! Each rule that its `Check` line, or a line of the list that two spaces indent right after it,
//! names a task in, as `ci: <task>`, or as `` `clippy` `` for `lint`, is in the "Enforces" cell
//! of a row that names the task in its "`just` task" cell, in the table under "Checks that
//! exist" in `docs/operations/ci.md`, by its ID or in a range `<ID> to <ID>` of its prefix
//! (DOC-12). A task that no row names is `named_checks`'.

use std::collections::BTreeMap;

use crate::{
    Finding,
    catalog::{document_in, rule_heading},
    markdown,
    repo::Repo,
    text,
};

/// The table of checks.
const CI: &str = "docs/operations/ci.md";

/// Each rule whose `Check` names a task whose row doesn't list it.
pub fn unlisted(repo: &Repo) -> Vec<Finding> {
    let mut rows: BTreeMap<String, String> = BTreeMap::new();
    let tables = repo.file(CI).map(|ci| markdown::tables(&ci.text));
    let tables = tables.into_iter().flatten();
    for table in tables.filter(|table| table.section.as_deref() == Some("Checks that exist")) {
        let at = |name: &str| table.header.iter().position(|cell| cell == name);
        let (Some(task), Some(enforces)) = (at("`just` task"), at("Enforces")) else {
            continue;
        };
        for (_, cells) in &table.rows {
            let (Some(tasks), Some(cell)) = (cells.get(task), cells.get(enforces)) else {
                continue;
            };
            for name in tasks.split('`').skip(1).step_by(2) {
                let row = rows.entry(name.to_owned()).or_default();
                row.push_str(cell);
                row.push('|');
            }
        }
    }
    let mut findings = Vec::new();
    let rules = repo
        .files()
        .filter(|file| document_in(&file.path, "docs/standards/").is_some());
    for file in rules {
        let (mut rule, mut check) = (None, false);
        for (index, line) in file.text.lines().enumerate() {
            if let Some(heading) = rule_heading(line) {
                (rule, check) = (Some(heading.id()), false);
                continue;
            }
            check = line.starts_with("- Check:") || (check && line.starts_with("  "));
            let Some(id) = rule.as_ref().filter(|_| check) else {
                continue;
            };
            let named = line.match_indices("ci: ").map(|(at, _)| {
                let rest = &line[at + 4..];
                let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
                rest[..end.unwrap_or(rest.len())].to_owned()
            });
            let clippy = line.contains("`clippy`").then(|| "lint".to_owned());
            for task in named.chain(clippy) {
                if rows.get(&task).is_some_and(|cell| !lists(cell, id)) {
                    findings.push(Finding::new(
                        &file.path,
                        index + 1,
                        format!(
                            "{id} names `{task}`, whose row in {CI} doesn't list it in its \
                             Enforces (DOC-12)"
                        ),
                    ));
                }
            }
        }
    }
    findings
}

/// Whether `cell` holds `id`, or a range `<ID> to <ID>` of its prefix that holds its number.
fn lists(cell: &str, id: &str) -> bool {
    let tokens = text::id_tokens(cell);
    let number = |digits: &str| digits.parse::<u32>().ok();
    tokens.iter().any(|token| token.id() == id)
        || tokens.windows(2).any(|pair| {
            let (from, to) = (&pair[0], &pair[1]);
            let within = id
                .strip_prefix(from.prefix)
                .and_then(|rest| rest.strip_prefix('-'))
                .and_then(number);
            cell[from.column..to.column] == format!("{} to ", from.id())
                && from.prefix == to.prefix
                && matches!(
                    (number(from.number), within, number(to.number)),
                    (Some(low), Some(n), Some(high)) if (low..=high).contains(&n)
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_12_a_task_that_a_rule_s_check_names_lists_the_rule_in_its_row() {
        let ci = "# CI\n\n## Checks that exist\n\n| Check | `just` task | Enforces |\n|---|---|---|\n\
                  | Lints | `lint` | STYLE-01 to STYLE-03, STYLE-07 (bans) |\n| Shapes | `check-shape` | STYLE-02 |\n";
        let rules = "# Style\n\n### STYLE-02 In the range\n- Check: `clippy`, plus `ci: check-shape`.\n\n\
                     ### STYLE-04 Named in the list alone\n- Check: `review`, each as below.\n  \
                     - `clippy` (a ban).\n  - `ci: check-shape` (a shape).\n  - `ci: check-later`.\n\
                     Not the list's, `ci: check-shape`.\n\n\
                     ### STYLE-07 Cited\n- Check: `clippy` (STYLE-08).\n";
        let repo = Repo::of(&[(CI, ci), ("docs/standards/style.md", rules)]);

        let found: Vec<String> = unlisted(&repo).iter().map(ToString::to_string).collect();

        assert_eq!(
            found,
            [
                "docs/standards/style.md:8: STYLE-04 names `lint`, whose row in docs/operations/ci.md doesn't list it in its Enforces (DOC-12)",
                "docs/standards/style.md:9: STYLE-04 names `check-shape`, whose row in docs/operations/ci.md doesn't list it in its Enforces (DOC-12)",
            ]
        );
    }
}
