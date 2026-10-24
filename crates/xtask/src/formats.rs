//! `check-docs`, the formats: specs, rules and every table keep the formats DOC-15 gives
//! them, and the tracker keeps the sections DOC-16 lists.

use crate::{
    Finding,
    catalog::document_in,
    markdown::{self, Table},
    repo::{File, Repo},
    text,
};

/// The spec README, which lists a spec's sections and indexes the specs.
const SPEC_README: &str = "docs/spec/README.md";

/// The standards README, which lists the rule files and the kinds of check.
const STANDARDS_README: &str = "docs/standards/README.md";

/// The documentation rules, where DOC-16 lists the tracker's sections.
const DOC_RULES: &str = "docs/standards/docs.md";

/// The migration tracker.
const TRACKER: &str = "docs/MIGRATION.md";

/// The columns of a table of behaviors (DOC-15).
const BEHAVIOR_COLUMNS: [&str; 4] = ["ID", "Behavior", "Rules", "Test prefix"];

/// The most words the tracker's Current work may hold (DOC-16).
const CURRENT_WORK_WORDS: usize = 200;

/// Runs the check.
pub fn check(repo: &Repo) -> Vec<Finding> {
    let mut findings = specs(repo);
    findings.extend(rules(repo));
    findings.extend(tracker(repo));
    findings.extend(table_rows(repo));
    findings
}

/// Every table in a document has a delimiter row under its header and a blank line after it,
/// and every row, the delimiter row among them, has as many cells as the header, so no row has
/// lost a cell or run into the next one (DOC-15).
fn table_rows(repo: &Repo) -> Vec<Finding> {
    let mut findings = Vec::new();
    for file in repo.files().filter(|file| file.path.ends_with(".md")) {
        for table in markdown::tables(&file.text) {
            let header = table.header.len();
            match table.delimiter {
                None => findings.push(Finding::new(
                    &file.path,
                    table.line,
                    "the table has no delimiter row under its header (DOC-15)",
                )),
                Some((line, count)) if count != header => findings.push(Finding::new(
                    &file.path,
                    line,
                    format!(
                        "the delimiter row has {count} {}, and its table's header has {header} (DOC-15)",
                        cells_noun(count)
                    ),
                )),
                Some(_) => {}
            }
            for (line, cells) in &table.rows {
                if cells.len() != header {
                    findings.push(Finding::new(
                        &file.path,
                        *line,
                        format!(
                            "the row has {} {}, and its table's header has {header} (DOC-15)",
                            cells.len(),
                            cells_noun(cells.len())
                        ),
                    ));
                }
            }
            let last = table
                .rows
                .last()
                .map(|(line, _)| *line)
                .or(table.delimiter.map(|(line, _)| line))
                .unwrap_or(table.line);
            if file
                .text
                .lines()
                .nth(last)
                .is_some_and(|next| !next.trim().is_empty())
            {
                findings.push(Finding::new(
                    &file.path,
                    last + 1,
                    "a table has a blank line after it, so no line runs on into it (DOC-15)",
                ));
            }
        }
    }
    findings
}

/// "cell" or "cells", for `count` of them.
fn cells_noun(count: usize) -> &'static str {
    if count == 1 { "cell" } else { "cells" }
}

/// The sections a kind of document has, in order, and where that list comes from.
struct Outline<'a> {
    /// Each section's name, and whether a document may leave it out.
    sections: Vec<(&'a str, bool)>,
    /// What lists the sections, such as "the spec README".
    source: &'a str,
    /// What the document is called in a finding, such as "the spec".
    document: &'a str,
    /// The rule that fixes the sections.
    rule: &'a str,
}

impl Outline<'_> {
    /// Each level-2 heading of `file` that the outline doesn't list or that comes out of order,
    /// then each section the outline requires that `file` lacks.
    fn findings(&self, file: &File) -> Vec<Finding> {
        let headings = markdown::sections(&file.text);
        let mut findings = Vec::new();
        let mut furthest: Option<usize> = None;
        for (line, name) in &headings {
            let Some(index) = self.sections.iter().position(|(listed, _)| listed == name) else {
                findings.push(Finding::new(
                    &file.path,
                    *line,
                    format!(
                        "`## {name}` isn't a section {} lists ({})",
                        self.source, self.rule
                    ),
                ));
                continue;
            };
            match furthest {
                Some(before) if index == before => findings.push(Finding::new(
                    &file.path,
                    *line,
                    format!("`## {name}` appears more than once ({})", self.rule),
                )),
                Some(before) if index < before => findings.push(Finding::new(
                    &file.path,
                    *line,
                    format!(
                        "`## {name}` is out of order: {} lists it before `## {}` ({})",
                        self.source, self.sections[before].0, self.rule
                    ),
                )),
                _ => furthest = Some(index),
            }
        }
        for (name, _) in self.sections.iter().filter(|(_, optional)| !optional) {
            if !headings.iter().any(|(_, heading)| heading == name) {
                findings.push(Finding::new(
                    &file.path,
                    1,
                    format!(
                        "{} has no `## {name}` section ({})",
                        self.document, self.rule
                    ),
                ));
            }
        }
        findings
    }
}

/// Each spec has the spec README's sections, its tables of behaviors have their columns and
/// test prefixes, and the README's index links it (DOC-15). A section whose description starts
/// with "Only" may be left out.
fn specs(repo: &Repo) -> Vec<Finding> {
    let Some(readme) = repo.file(SPEC_README) else {
        return Vec::new();
    };
    let outline = Outline {
        sections: markdown::named_items(&readme.text, "## Sections")
            .into_iter()
            .map(|(name, rest)| (name, rest.starts_with("Only")))
            .collect(),
        source: "the spec README",
        document: "the spec",
        rule: "DOC-15",
    };
    let index = markdown::under(&readme.text, "## Specs").map_or_else(String::new, |(_, lines)| {
        lines
            .iter()
            .map(|(_, line)| *line)
            .collect::<Vec<_>>()
            .join("\n")
    });
    let mut findings = Vec::new();
    for file in repo.files() {
        let Some(module) = document_in(&file.path, "docs/spec/") else {
            continue;
        };
        findings.extend(outline.findings(file));
        for table in markdown::tables(&file.text) {
            findings.extend(behavior_table(&file.path, &table));
        }
        if !index.contains(&format!("]({module}.md)")) {
            findings.push(Finding::new(
                &file.path,
                1,
                "the spec README's index doesn't link this spec (DOC-15)",
            ));
        }
    }
    findings
}

/// A table whose first column is `ID`, or whose rows start with a behavior ID, is a table of
/// behaviors: it sits in the Behaviors section, has the four columns, and each test prefix
/// matches its ID (DOC-15). A row without the four cells is `table_rows`'s finding.
fn behavior_table(path: &str, table: &Table) -> Vec<Finding> {
    let defines_behaviors = table
        .rows
        .iter()
        .any(|(_, cells)| cells.first().is_some_and(|cell| is_behavior_id(cell)));
    if table.header.first().map(String::as_str) != Some("ID") && !defines_behaviors {
        return Vec::new();
    }
    let mut findings = Vec::new();
    if table.section.as_deref() != Some("Behaviors") {
        findings.push(Finding::new(
            path,
            table.line,
            "a table of behaviors belongs in the `## Behaviors` section (DOC-15)",
        ));
    }
    if table.header != BEHAVIOR_COLUMNS {
        findings.push(Finding::new(
            path,
            table.line,
            "a table of behaviors has the columns ID, Behavior, Rules and Test prefix (DOC-15)",
        ));
        return findings;
    }
    for (line, cells) in &table.rows {
        let (Some(id), Some(prefix)) = (cells.first(), cells.get(3)) else {
            continue;
        };
        let expected = format!("`{}_`", id.to_lowercase().replace('-', "_"));
        if *prefix != expected {
            findings.push(Finding::new(
                path,
                *line,
                format!("the test prefix of {id} is {expected} (DOC-15)"),
            ));
        }
    }
    findings
}

/// Whether `cell` is a behavior ID and nothing else, as the catalog reads one.
fn is_behavior_id(cell: &str) -> bool {
    text::id_tokens(cell).first().is_some_and(|token| {
        token.column == 0 && token.number.len() == 3 && token.id().len() == cell.len()
    })
}

/// Each rule file is in the standards README's table of files, and each of its rules uses the
/// file's prefix and has the README's format (DOC-15).
fn rules(repo: &Repo) -> Vec<Finding> {
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
                format!("the standards README's table of files doesn't list `{name}` (DOC-15)"),
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
            block.body.push((index + 1, line));
        }
    }
    blocks
}

/// The findings for one rule: its heading names an ID with the file's prefix, and its body is
/// a statement, then one `Why` line, then one `Check` line that starts with a kind of check.
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
                "`### {}` isn't a rule heading: a rule ID, then the rule's name (DOC-15)",
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
        if why + 1 != *check || check + 1 != block.body.len() {
            problems.push(format!(
                "{id} doesn't end with its `Why` line, then its `Check` line"
            ));
        }
    }
    let mut findings: Vec<Finding> = problems
        .into_iter()
        .map(|problem| Finding::new(path, block.line, format!("{problem} (DOC-15)")))
        .collect();
    if let [check] = checks.as_slice() {
        let (line, text) = block.body[*check];
        if !starts_with_kind(text.trim_start_matches("- Check:").trim_start(), kinds) {
            findings.push(Finding::new(
                path,
                line,
                format!(
                    "{id}'s `Check` line doesn't start with a kind of check the standards README lists (DOC-15)"
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

/// The tracker has the sections DOC-16 lists, in order, and its Current work holds at most
/// `CURRENT_WORK_WORDS` words (DOC-16).
fn tracker(repo: &Repo) -> Vec<Finding> {
    let Some(tracker) = repo.file(TRACKER) else {
        return Vec::new();
    };
    let listed = repo
        .file(DOC_RULES)
        .map(|rules| markdown::named_items(&rules.text, "### DOC-16"))
        .unwrap_or_default();
    let outline = Outline {
        sections: listed.into_iter().map(|(name, _)| (name, false)).collect(),
        source: "DOC-16",
        document: "the tracker",
        rule: "DOC-16",
    };
    let mut findings = outline.findings(tracker);
    let mut lines = tracker.text.lines().enumerate();
    if let Some((index, _)) = lines.find(|(_, line)| line.trim_end() == "## Current work") {
        // Every word up to the next section counts, in subheadings and code blocks too. A
        // line inside a code block never starts the next section.
        let mut in_code = false;
        let words: usize = lines
            .take_while(|(_, text)| {
                if text.trim_start().starts_with("```") {
                    in_code = !in_code;
                }
                in_code || !text.starts_with("## ")
            })
            .map(|(_, text)| text.split_whitespace().count())
            .sum();
        let line = index + 1;
        if words > CURRENT_WORK_WORDS {
            findings.push(Finding::new(
                TRACKER,
                line,
                format!(
                    "Current work has {words} words, and DOC-16 allows {CURRENT_WORK_WORDS} (DOC-16)"
                ),
            ));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::fixture_with;

    const TRACKER: &str = "docs/MIGRATION.md";

    fn findings(changes: &[(&'static str, &'static str)]) -> Vec<String> {
        check(&Repo::of(&fixture_with(changes)))
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn doc_15_the_fixture_keeps_every_format() {
        let tracker = "# Tracker\n\n## Current work\n\nNext.\n\n## Decisions\n";

        assert_eq!(findings(&[(TRACKER, tracker)]), Vec::<String>::new());
    }

    #[test]
    fn doc_15_every_row_of_a_table_has_its_headers_cells() {
        let notes = "# Notes\n\n| A | B |\n|---|---|\n| one | two |\n\
                     | three || four | five |\n| `a \\| b` | six |\n| seven |\n\
                     | eight | nine ||\n| ten | eleven \\|\n\n\
                     | C | D |\n|---|\n\n| E |\n| e |\n\n\
                     - A list:\n\n  | F | G |\n  |---|---|\n  | f |\n  | g | h \\\\| i |\n\
                     Run on.\n";

        assert_eq!(
            findings(&[("docs/notes.md", notes)]),
            [
                "docs/notes.md:6: the row has 4 cells, and its table's header has 2 (DOC-15)",
                "docs/notes.md:8: the row has 1 cell, and its table's header has 2 (DOC-15)",
                "docs/notes.md:9: the row has 3 cells, and its table's header has 2 (DOC-15)",
                "docs/notes.md:13: the delimiter row has 1 cell, and its table's header has 2 (DOC-15)",
                "docs/notes.md:15: the table has no delimiter row under its header (DOC-15)",
                "docs/notes.md:22: the row has 1 cell, and its table's header has 2 (DOC-15)",
                "docs/notes.md:24: a table has a blank line after it, so no line runs on into it (DOC-15)"
            ]
        );
    }

    #[test]
    fn doc_15_a_spec_section_that_is_unknown_out_of_order_or_missing_is_reported() {
        let reordered = "# Spec\n\n## Behaviors\n\n## Purpose\n\n## Notes\n";
        let short = "# Spec\n\n## Purpose\n\nGadgets.\n";

        assert_eq!(
            findings(&[
                ("docs/spec/widgets.md", reordered),
                ("docs/spec/gadgets.md", short)
            ]),
            [
                "docs/spec/widgets.md:5: `## Purpose` is out of order: the spec README lists it before `## Behaviors` (DOC-15)",
                "docs/spec/widgets.md:7: `## Notes` isn't a section the spec README lists (DOC-15)",
                "docs/spec/gadgets.md:1: the spec has no `## Behaviors` section (DOC-15)",
                "docs/spec/gadgets.md:1: the spec README's index doesn't link this spec (DOC-15)"
            ]
        );
    }

    #[test]
    fn doc_15_a_table_of_behaviors_has_its_columns_its_section_and_matching_test_prefixes() {
        let spec = "# Spec\n\n## Purpose\n\n\
                    | ID | Behavior |\n|---|---|\n| WIDGETS-003 | Misplaced |\n\n\
                    ## Behaviors\n\n\
                    | ID | Behavior | Rules | Test prefix |\n|---|---|---|---|\n\
                    | WIDGETS-001 | One | STYLE-01 | `widgets_01_` |\n\
                    | WIDGETS-002 | Two | STYLE-01 | widgets_002_ |\n";

        assert_eq!(
            findings(&[("docs/spec/widgets.md", spec)]),
            [
                "docs/spec/widgets.md:5: a table of behaviors belongs in the `## Behaviors` section (DOC-15)",
                "docs/spec/widgets.md:5: a table of behaviors has the columns ID, Behavior, Rules and Test prefix (DOC-15)",
                "docs/spec/widgets.md:13: the test prefix of WIDGETS-001 is `widgets_001_` (DOC-15)",
                "docs/spec/widgets.md:14: the test prefix of WIDGETS-002 is `widgets_002_` (DOC-15)"
            ]
        );
    }

    #[test]
    fn doc_15_a_rule_has_a_statement_then_a_why_line_then_a_check_line_that_starts_with_a_kind() {
        let rules = "# Style\n\n\
                     ### STYLE-01 A rule\n- Why: a reason.\n- Check: `ci: check-style`.\n\n\
                     ### STYLE-02 Another\nThe statement.\n- Check: `review`.\n- Why: late.\n\n\
                     ### STYLE-03 A third\nThe statement.\n- Why: a reason.\n- Check: tested by `test`.\n\n\
                     ### STYLE-04 A fourth\nThe statement.\n- Check: `ci:` alone.\n\n\
                     ### Not a rule\n";

        assert_eq!(
            findings(&[("docs/standards/style.md", rules)]),
            [
                "docs/standards/style.md:3: STYLE-01 has no statement before its `Why` line (DOC-15)",
                "docs/standards/style.md:7: STYLE-02 doesn't end with its `Why` line, then its `Check` line (DOC-15)",
                "docs/standards/style.md:15: STYLE-03's `Check` line doesn't start with a kind of check the standards README lists (DOC-15)",
                "docs/standards/style.md:17: STYLE-04 has no `Why` line (DOC-15)",
                "docs/standards/style.md:19: STYLE-04's `Check` line doesn't start with a kind of check the standards README lists (DOC-15)",
                "docs/standards/style.md:21: `### Not a rule` isn't a rule heading: a rule ID, then the rule's name (DOC-15)"
            ]
        );
    }

    #[test]
    fn doc_15_a_rule_file_is_in_the_standards_readme_and_its_rules_use_its_prefix() {
        let misplaced = "# Style\n\n### DOC-16 Misplaced\nThe statement.\n- Why: a reason.\n- Check: `review`.\n";
        let unlisted = "# Shape\n\n### SHAPE-01 A rule\nThe statement.\n- Why: a reason.\n- Check: `review`.\n";

        assert_eq!(
            findings(&[
                ("docs/standards/style.md", misplaced),
                ("docs/standards/shape.md", unlisted)
            ]),
            [
                "docs/standards/style.md:3: DOC-16 is in `style.md`, whose rules start with STYLE- (DOC-15)",
                "docs/standards/shape.md:1: the standards README's table of files doesn't list `shape.md` (DOC-15)"
            ]
        );
    }

    #[test]
    fn doc_16_the_tracker_has_the_sections_doc_16_lists_in_order() {
        let reordered = "# Tracker\n\n## Decisions\n\n## Current work\n\nNext.\n\n## Notes\n";
        let short = "# Tracker\n\n## Current work\n\nNext.\n";

        assert_eq!(
            findings(&[(TRACKER, reordered)]),
            [
                "docs/MIGRATION.md:5: `## Current work` is out of order: DOC-16 lists it before `## Decisions` (DOC-16)",
                "docs/MIGRATION.md:9: `## Notes` isn't a section DOC-16 lists (DOC-16)"
            ]
        );
        assert_eq!(
            findings(&[(TRACKER, short)]),
            ["docs/MIGRATION.md:1: the tracker has no `## Decisions` section (DOC-16)"]
        );
    }

    #[test]
    fn doc_16_current_work_holds_at_most_200_words() {
        let tracker = |words: usize| {
            let text = format!(
                "# Tracker\n\n## Current work\n\n{}\n\n## Decisions\n",
                vec!["word"; words].join(" ")
            );
            &*Box::leak(text.into_boxed_str())
        };

        assert_eq!(findings(&[(TRACKER, tracker(200))]), Vec::<String>::new());
        assert_eq!(
            findings(&[(TRACKER, tracker(201))]),
            ["docs/MIGRATION.md:3: Current work has 201 words, and DOC-16 allows 200 (DOC-16)"]
        );
    }

    #[test]
    fn doc_16_current_work_counts_every_word_up_to_the_next_section() {
        let words = vec!["word"; 100].join(" ");
        let text = format!(
            "# Tracker\n\n## Current work\n\n{words}\n\n### More\n\n```\n{words}\n```\n\n## Decisions\n"
        );

        assert_eq!(
            findings(&[(TRACKER, Box::leak(text.into_boxed_str()))]),
            ["docs/MIGRATION.md:3: Current work has 204 words, and DOC-16 allows 200 (DOC-16)"]
        );
    }

    #[test]
    fn doc_16_a_heading_inside_a_code_block_doesnt_end_current_work() {
        let words = vec!["word"; 100].join(" ");
        let text = format!(
            "# Tracker\n\n## Current work\n\n{words}\n\n```\n## a shell comment\n{words}\n```\n\n## Decisions\n"
        );

        assert_eq!(
            findings(&[(TRACKER, Box::leak(text.into_boxed_str()))]),
            ["docs/MIGRATION.md:3: Current work has 206 words, and DOC-16 allows 200 (DOC-16)"]
        );
    }

    #[test]
    fn doc_15_every_table_that_defines_behaviors_has_their_columns() {
        let spec = "# Spec\n\n## Purpose\n\nWidgets.\n\n## Behaviors\n\n\
                    | Id | Behavior |\n|---|---|\n| WIDGETS-001 | One |\n\n\
                    | ID | Behavior | Rules | Test prefix |\n|---|---|---|---|\n| WIDGETS-002 | Two |\n";

        assert_eq!(
            findings(&[("docs/spec/widgets.md", spec)]),
            [
                "docs/spec/widgets.md:9: a table of behaviors has the columns ID, Behavior, Rules and Test prefix (DOC-15)",
                "docs/spec/widgets.md:15: the row has 2 cells, and its table's header has 4 (DOC-15)"
            ]
        );
    }

    #[test]
    fn doc_15_a_rule_heading_names_the_rule() {
        let rules =
            "# Style\n\n### STYLE-01\nThe statement.\n- Why: a reason.\n- Check: `review`.\n";

        assert_eq!(
            findings(&[("docs/standards/style.md", rules)]),
            [
                "docs/standards/style.md:3: the heading of STYLE-01 has no name after the ID (DOC-15)"
            ]
        );
    }
}
