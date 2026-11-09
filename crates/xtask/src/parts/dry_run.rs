//! `check-parts`: what `just remove` reads and checks before it changes a file, for each run of
//! `parts.yaml`, done in memory, so that a marker that formatting moved or a line that names a
//! part without its marker fails `just checks` in seconds, rather than `check-removable`'s copy
//! on `main` (ARCH-07).

use std::collections::HashSet;

use super::{LIST, Part, Run, is_migration, leftovers, list, markers, owned, validate};
use crate::{Finding, Outcome, metrics, repo::Repo};

/// The files that a removal writes again, or prunes, from the code it kept, rather than deleting
/// their marked lines: `Cargo.lock`, `openapi.json`, the metric catalog and the query cache. The
/// dry run reads no word in them.
const REWRITTEN: [&str; 4] = ["Cargo.lock", "openapi.json", metrics::CATALOG, ".sqlx/"];

/// The tracker, whose table of decisions gives each decision a row that starts `| D<number> |`.
const TRACKER: &str = "docs/MIGRATION.md";

/// Checks the parts' markers and files, each Rust marker standing alone on its line, and each
/// run's removal, done in memory (ARCH-07).
pub fn check(repo: &Repo) -> Outcome {
    let (parts, runs) = match list(repo) {
        Ok(list) => list,
        Err(error) => {
            return Outcome {
                report: Vec::new(),
                findings: vec![Finding::new(LIST, 1, error)],
            };
        }
    };
    let mut findings = validate(repo, &parts);
    for file in repo
        .files()
        .filter(|file| file.path.ends_with(".rs") && !is_migration(&file.path))
    {
        for line in markers::misplaced_in_rust(&file.text) {
            findings.push(Finding::new(
                &file.path,
                line,
                "a marker in Rust is a `//` comment alone on its line, opening or closing a block, \
                 where rustfmt leaves it; what a part adds to a list of short elements goes in a \
                 statement of its own (ARCH-07)",
            ));
        }
        for line in markers::attribute_above_block(&file.text) {
            findings.push(Finding::new(
                &file.path,
                line,
                "this attribute or doc comment belongs to the first item of the part's block \
                 below it, so removing the part would leave it on the item after the block: \
                 move it inside the block (ARCH-07)",
            ));
        }
    }
    for file in repo.files() {
        for line in markers::text_after_marker(&file.text) {
            findings.push(Finding::new(
                &file.path,
                line,
                "removing the part deletes this whole line, the text after its marker too: give \
                 the part's text a line of its own, with the marker at its end (ARCH-07)",
            ));
        }
    }
    for run in &runs {
        for name in &run.parts {
            if !parts.iter().any(|part| &part.name == name) {
                findings.push(Finding::new(
                    LIST,
                    1,
                    format!("the run `{}` removes `{name}`, which is no part", run.name),
                ));
            }
        }
    }
    let mut report = Vec::new();
    // A marker that `validate` refused leaves no removal to run.
    if findings.is_empty() {
        for run in &runs {
            if run.parts.is_empty() {
                report.push(format!(
                    "{}: every part it removes is removed already",
                    run.name
                ));
                continue;
            }
            let (summary, found) = remove(repo, &parts, run);
            report.push(summary);
            findings.extend(found);
        }
    }
    Outcome { report, findings }
}

/// Removes `run`'s parts from `repo` in memory, and returns what it removed, and a finding for
/// each Rust file whose kept lines don't parse, and for each kept path or line, other than those
/// of the files the removal writes again, that holds a word of a part it removes.
fn remove(repo: &Repo, parts: &[Part], run: &Run) -> (String, Vec<Finding>) {
    let known: Vec<&str> = parts.iter().map(|part| part.name.as_str()).collect();
    let removed: Vec<&Part> = parts
        .iter()
        .filter(|part| run.parts.contains(&part.name))
        .collect();
    let removing: Vec<&str> = removed.iter().map(|part| part.name.as_str()).collect();
    let owned: HashSet<String> = removed.iter().flat_map(|part| owned(repo, part)).collect();
    let mut findings = Vec::new();
    let mut kept = Vec::new();
    let mut edited = 0;
    for file in repo.files().filter(|file| !owned.contains(&file.path)) {
        let Ok(lines) = markers::kept(&file.text, &removing, &known) else {
            continue;
        };
        if lines.len() < file.text.lines().count() {
            edited += 1;
            if file.path.ends_with(".rs") {
                findings.extend(unparsed(&file.path, &lines));
            }
        }
        if !is_rewritten(&file.path) {
            kept.push((file.path.as_str(), lines));
        }
    }
    findings.extend(dangling_decisions(repo, &kept));
    let paths = repo.paths().filter(|path| !owned.contains(*path));
    findings.extend(leftovers(paths, kept.into_iter(), &removed));
    for finding in &mut findings {
        finding.message = format!(
            "removing the run `{}`'s parts: {}",
            run.name, finding.message
        );
    }
    let summary = format!(
        "{}: removing {} deletes {} files and edits {edited}",
        run.name,
        removing.join(", "),
        owned.len(),
    );
    (summary, findings)
}

/// A finding for each of the `kept` lines that cites a decision, `D` and its number, whose row in
/// the tracker the removal deletes, since the citation would point at nothing: the removal deletes
/// every ID that points into what it removes (ARCH-07), and check-docs resolves no decision.
fn dangling_decisions(repo: &Repo, kept: &[(&str, Vec<(usize, &str)>)]) -> Vec<Finding> {
    let Some(tracker) = repo.file(TRACKER) else {
        return Vec::new();
    };
    let left: HashSet<usize> = kept
        .iter()
        .filter(|(path, _)| *path == TRACKER)
        .flat_map(|(_, lines)| lines.iter().map(|(number, _)| *number))
        .collect();
    let removed: Vec<&str> = tracker
        .text
        .lines()
        .enumerate()
        .filter(|(index, _)| !left.contains(&(index + 1)))
        .filter_map(|(_, line)| decision(line.strip_prefix("| ")?))
        .collect();
    let mut findings = Vec::new();
    for (path, lines) in kept {
        for (number, line) in lines {
            for id in removed.iter().filter(|id| cites(line, id)) {
                findings.push(Finding::new(
                    path,
                    *number,
                    format!(
                        "cites {id}, whose decision the removal deletes from the tracker (ARCH-07)"
                    ),
                ));
            }
        }
    }
    findings
}

/// The decision ID that `text` starts with, such as `D7` in `D7 | ...`.
fn decision(text: &str) -> Option<&str> {
    let digits = text
        .strip_prefix('D')?
        .find(|c: char| !c.is_ascii_digit())
        .filter(|length| *length > 0)?;
    text[1 + digits..]
        .starts_with(" |")
        .then(|| &text[..=digits])
}

/// Whether `line` cites the decision `id` as a word of its own, so `D12` doesn't cite `D1`.
fn cites(line: &str, id: &str) -> bool {
    line.match_indices(id).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + id.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric())
            && !after.is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

/// Whether the removal writes the file at `path` again, or prunes it, rather than deleting its
/// marked lines.
fn is_rewritten(path: &str) -> bool {
    REWRITTEN.iter().any(|rewritten| {
        path == *rewritten || (rewritten.ends_with('/') && path.starts_with(rewritten))
    })
}

/// A finding at the line where Rust made of `lines` stops parsing, counted in the file the lines
/// were kept from, if it does.
fn unparsed(path: &str, lines: &[(usize, &str)]) -> Option<Finding> {
    let text = lines
        .iter()
        .map(|(_, line)| *line)
        .collect::<Vec<_>>()
        .join("\n");
    let error = syn::parse_file(&text).err()?;
    let at = error.span().start().line;
    let line = lines
        .get(at.saturating_sub(1))
        .map_or(1, |(number, _)| *number);
    Some(Finding::new(
        path,
        line,
        format!("the Rust left doesn't parse here: {error}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marker of `part` in Rust, which the source of these tests builds rather than holds, so
    /// `just remove` never reads one here.
    fn mark(part: &str) -> String {
        format!("// {} {part}", "part:")
    }

    fn end(part: &str) -> String {
        format!("// end {} {part}", "part:")
    }

    /// The list of one part, `offers`, which owns two files, and of the run that removes it, with
    /// the part's markers, as `parts.yaml` carries them.
    fn list() -> String {
        [
            "parts:".to_owned(),
            format!("  #{}", &mark("offers")[2..]),
            "  - name: offers".to_owned(),
            "    paths:".to_owned(),
            "      - src/offers.rs".to_owned(),
            "      - src/offers_more.rs".to_owned(),
            "    words:".to_owned(),
            "      - offer".to_owned(),
            format!("  #{}", &end("offers")[2..]),
            "runs:".to_owned(),
            "  - name: shop".to_owned(),
            "    parts:".to_owned(),
            format!("      - offers #{}", &mark("offers")[2..]),
            String::new(),
        ]
        .join("\n")
    }

    #[test]
    fn arch_07_a_removal_that_cuts_a_statement_fails_at_its_line() {
        let code = format!(
            "fn a() {{\n    let x = [\n        1,\n{}\n        2,\n    ];\n{}\n}}\n",
            mark("offers"),
            end("offers")
        );
        let list = list();
        let repo = Repo::of(&[(LIST, &list), ("src/a.rs", &code), ("src/offers.rs", "")]);

        let outcome = check(&repo);

        let places: Vec<(&str, usize)> = outcome
            .findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line))
            .collect();
        assert_eq!(places, [("src/a.rs", 8)], "{:?}", outcome.findings);
    }

    #[test]
    fn arch_07_a_kept_line_that_names_a_removed_part_fails_but_not_in_a_rewritten_file() {
        let code = format!(
            "fn a() {{}}\n{}\nfn b() {{}}\n{}\n// An offer.\n",
            mark("offers"),
            end("offers")
        );
        let list = list();
        let repo = Repo::of(&[
            (LIST, &list),
            ("src/a.rs", &code),
            ("src/offers.rs", "fn offer() {}"),
            ("src/offers_more.rs", ""),
            ("openapi.json", "{\"offers\": {}}"),
            (".sqlx/query-1.json", "{\"query\": \"SELECT offer\"}"),
        ]);

        let outcome = check(&repo);

        let places: Vec<(&str, usize)> = outcome
            .findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line))
            .collect();
        assert_eq!(places, [("src/a.rs", 5)], "{:?}", outcome.findings);
    }

    #[test]
    fn arch_07_a_kept_line_that_cites_a_decision_the_removal_deletes_fails() {
        let marker = format!("<!-- {} offers -->", "part:");
        let tracker = format!(
            "| ID | Decision | Status |\n|---|---|---|\n| D1 | Kept | accepted |\n\
             | D2 | Removed | accepted {marker} |\n"
        );
        let doc =
            format!("As D2 says.\nAs D1 says.\nD2 again. {marker}\nD20 and XD2 are others.\n");
        let list = list();
        let repo = Repo::of(&[
            (LIST, &list),
            (TRACKER, &tracker),
            ("docs/a.md", &doc),
            ("src/offers.rs", ""),
        ]);

        let outcome = check(&repo);

        let places: Vec<(&str, usize)> = outcome
            .findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line))
            .collect();
        assert_eq!(places, [("docs/a.md", 1)], "{:?}", outcome.findings);
    }

    #[test]
    fn arch_07_a_run_reports_the_files_it_deletes_and_edits() {
        let code = format!(
            "fn a() {{}}\n{}\nfn b() {{}}\n{}\n",
            mark("offers"),
            end("offers")
        );
        let list = list();
        let repo = Repo::of(&[
            (LIST, &list),
            ("src/a.rs", &code),
            ("src/b.rs", "fn c() {}\n"),
            ("src/offers.rs", ""),
            ("src/offers_more.rs", ""),
        ]);

        let outcome = check(&repo);

        assert_eq!(
            outcome.report,
            ["shop: removing offers deletes 2 files and edits 2"]
        );
    }

    #[test]
    fn arch_07_a_run_that_removes_no_part_fails() {
        let repo = Repo::of(&[(
            LIST,
            "parts:\nruns:\n  - name: shop\n    parts:\n      - offers\n",
        )]);

        let outcome = check(&repo);

        let places: Vec<(&str, usize)> = outcome
            .findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line))
            .collect();
        assert_eq!(places, [(LIST, 1)], "{:?}", outcome.findings);
    }

    #[test]
    fn arch_07_a_run_whose_parts_are_removed_already_is_skipped() {
        let repo = Repo::of(&[(LIST, "parts:\nruns:\n  - name: none\n    parts:\n")]);

        let outcome = check(&repo);

        assert_eq!(
            outcome.report,
            ["none: every part it removes is removed already"]
        );
    }

    #[test]
    fn arch_07_a_marker_after_rust_code_fails() {
        let code = format!("fn a() {{}}\nfn b() {{}} {}\n", mark("offers"));
        let list = list();
        let repo = Repo::of(&[(LIST, &list), ("src/a.rs", &code), ("src/offers.rs", "")]);

        let outcome = check(&repo);

        let places: Vec<(&str, usize)> = outcome
            .findings
            .iter()
            .map(|finding| (finding.path.as_str(), finding.line))
            .collect();
        assert_eq!(places, [("src/a.rs", 2)], "{:?}", outcome.findings);
    }
}
