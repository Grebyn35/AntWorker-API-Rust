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
    for file in repo
        .files()
        .filter(|file| !owned.contains(&file.path) && !is_migration(&file.path))
    {
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
