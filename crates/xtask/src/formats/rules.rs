//! `check-docs`, the rules' format: each rule file is in the standards README's table of files,
//! and each of its rules uses the file's prefix and has the README's format (DOC-08).

use crate::{
    Finding,
    catalog::document_in,
    markdown::{self, Table},
    repo::Repo,
    text,
};

/// The standards README, which lists the rule files and the kinds of check.
const STANDARDS_README: &str = "docs/standards/README.md";

/// Each rule file is in the standards README's table of files, and each of its rules uses the
/// file's prefix and has the README's format (DOC-08).
pub fn check(repo: &Repo) -> Vec<Finding> {
    let Some(readme) = repo.file(STANDARDS_README) else {
        return Vec::new();
    };
    let tables = markdown::tables(&readme.text);
    let prefixes: Vec<(String, String)> = rows_in(&tables, "Files")
        .filter_map(|cells| {
            let prefix = cells.first()?.trim_matches('`').to_owned();
            let target = cells.get(1)?.split_once("](")?.1.trim_end_matches(')');
            Some((target.to_owned(), prefix))
        })
        .collect();
    let kinds: Vec<String> = rows_in(&tables, "Rule format")
        .filter_map(|cells| {
            let kind = cells.first()?.strip_prefix('`')?.strip_suffix('`')?;
            Some(kind.split('<').next().unwrap_or(kind).to_owned())
        })
        .collect();
    let mut findings = Vec::new();
    for file in repo.files() {
        let Some(name) = document_in(&file.path, "docs/standards/") else {
            continue;
        };
        let name = format!("{name}.md");
        let prefix = prefixes
            .iter()
            .find(|(target, _)| *target == name)
            .map(|(_, prefix)| prefix.as_str());
        if prefix.is_none() {
            findings.push(Finding::new(
                &file.path,
                1,
                format!("the standards README's table of files doesn't list `{name}` (DOC-08)"),
            ));
        }
        for block in rule_blocks(&file.text) {
            findings.extend(rule(&file.path, &name, prefix, &kinds, &block));
        }
    }
    findings
}

/// The cells of every row of the tables in `section`.
fn rows_in<'a>(tables: &'a [Table], section: &'a str) -> impl Iterator<Item = &'a Vec<String>> {
    tables
        .iter()
        .filter(move |table| table.section.as_deref() == Some(section))
        .flat_map(|table| &table.rows)
        .map(|(_, cells)| cells)
}

/// A level-3 heading of a rule file, and the lines under it up to the next heading.
struct Block<'a> {
    line: usize,
    heading: &'a str,
    body: Vec<(usize, &'a str)>,
}

fn rule_blocks(text: &str) -> Vec<Block<'_>> {
    let mut blocks: Vec<Block<'_>> = Vec::new();
    let mut open = false;
    for (index, line) in text.lines().enumerate() {
        if let Some(heading) = line.strip_prefix("### ") {
            blocks.push(Block {
                line: index + 1,
                heading,
                body: Vec::new(),
            });
            open = true;
        } else if line.starts_with('#') {
            open = false;
        } else if open
            && !line.trim().is_empty()
            && let Some(block) = blocks.last_mut()
        {
            // part: removal
            if crate::parts::unmarked(line).is_none() {
                continue;
            }
            // end part: removal
            block.body.push((index + 1, line));
        }
    }
    blocks
}

/// The findings for one rule: its heading names an ID with the file's prefix, and its body is
/// a statement, then one `Why` line, then one `Check` line that starts with a kind of check,
/// then nothing but lines that two spaces indent, as the items of a list nested under it are.
fn rule(
    path: &str,
    file: &str,
    prefix: Option<&str>,
    kinds: &[String],
    block: &Block<'_>,
) -> Vec<Finding> {
    let token = text::id_tokens(block.heading)
        .into_iter()
        .next()
        .filter(|token| token.column == 0 && token.number.len() == 2);
    let Some(token) = token else {
        return vec![Finding::new(
            path,
            block.line,
            format!(
                "`### {}` isn't a rule heading: a rule ID, then the rule's name (DOC-08)",
                block.heading
            ),
        )];
    };
    let id = token.id();
    let mut problems = Vec::new();
    if block.heading[id.len()..].trim().is_empty() {
        problems.push(format!("the heading of {id} has no name after the ID"));
    }
    if let Some(prefix) = prefix.filter(|prefix| *prefix != token.prefix) {
        problems.push(format!(
            "{id} is in `{file}`, whose rules start with {prefix}-"
        ));
    }
    let whys: Vec<usize> = positions(&block.body, "- Why:");
    let checks: Vec<usize> = positions(&block.body, "- Check:");
    for (found, name) in [(&whys, "Why"), (&checks, "Check")] {
        match found.len() {
            0 => problems.push(format!("{id} has no `{name}` line")),
            1 => {}
            _ => problems.push(format!("{id} has more than one `{name}` line")),
        }
    }
    if let ([why], [check]) = (whys.as_slice(), checks.as_slice()) {
        if *why == 0 {
            problems.push(format!("{id} has no statement before its `Why` line"));
        }
        // Only the items of a list nested under the `Check` line follow it.
        let nested = block.body[check + 1..]
            .iter()
            .all(|(_, line)| line.starts_with("  "));
        if why + 1 != *check || !nested {
            problems.push(format!(
                "{id} doesn't end with its `Why` line, then its `Check` line"
            ));
        }
    }
    let mut findings: Vec<Finding> = problems
        .into_iter()
        .map(|problem| Finding::new(path, block.line, format!("{problem} (DOC-08)")))
        .collect();
    if let [check] = checks.as_slice() {
        let (line, text) = block.body[*check];
        if !starts_with_kind(text.trim_start_matches("- Check:").trim_start(), kinds) {
            findings.push(Finding::new(
                path,
                line,
                format!(
                    "{id}'s `Check` line doesn't start with a kind of check the standards README lists (DOC-08)"
                ),
            ));
        }
    }
    findings
}

/// The positions in `body` of the lines that start with `marker`.
fn positions(body: &[(usize, &str)], marker: &str) -> Vec<usize> {
    body.iter()
        .enumerate()
        .filter(|(_, (_, line))| line.starts_with(marker))
        .map(|(position, _)| position)
        .collect()
}

/// Whether `check` starts with a code span naming one of `kinds`. A kind that ends with a
/// space, such as `ci: `, is a prefix that a name completes.
fn starts_with_kind(check: &str, kinds: &[String]) -> bool {
    let Some(span) = check
        .strip_prefix('`')
        .and_then(|rest| rest.split('`').next())
    else {
        return false;
    };
    kinds.iter().any(|kind| {
        if kind.ends_with(' ') {
            span.len() > kind.len() && span.starts_with(kind.as_str())
        } else {
            span == kind
        }
    })
}
