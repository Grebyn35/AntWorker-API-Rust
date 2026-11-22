//! `check-parts`: what `just remove` and `just keep` read and check before they change a file,
//! for each run of `parts.yaml`, and for each part alone, done in memory, so that a marker that
//! formatting moved or a line that names a part without its marker fails `just checks` within
//! a minute, rather than `check-removable`'s copy on `main`, or a fork's own removal (ARCH-07).

use std::collections::{HashMap, HashSet};

use super::{
    LIST, Part, REMOVAL, Run, citations, exports, is_migration, leftovers, list, markers, matrix,
    owned, validate,
};
use crate::{
    Finding, Outcome, metrics,
    repo::{File, Repo},
};

/// The files that a removal writes again, or prunes, from the code it kept, rather than deleting
/// their marked lines: `Cargo.lock`, `openapi.json`, the metric catalog and the query cache. The
/// dry run reads no word in them.
const REWRITTEN: [&str; 4] = ["Cargo.lock", "openapi.json", metrics::CATALOG, ".sqlx/"];

/// Checks the parts' markers and files, each Rust marker standing alone on its line, and each
/// run's removal, and each part's alone, done in memory (ARCH-07).
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
        for line in markers::block_arm_that_can_be_last(&file.text) {
            findings.push(Finding::new(
                &file.path,
                line,
                "this block is the body of a part's arm of a match that no arm of the core \
                 follows: once a removal leaves the arm its match's last, rustfmt moves the \
                 part's end marker onto this line; write the arm as an expression, or before an \
                 arm of the core (ARCH-07)",
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
    if let (Some(list), Some(workflow)) = (repo.file(LIST), repo.file(matrix::WORKFLOW)) {
        for found in matrix::disagreements(&list.text, &workflow.text) {
            findings.push(Finding::new(matrix::WORKFLOW, 1, found));
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
    // A marker that `validate` refused leaves no removal to run.
    let report = if findings.is_empty() {
        let (report, found) = removals(repo, &parts, &runs);
        findings.extend(found);
        report
    } else {
        Vec::new()
    };
    Outcome { report, findings }
}

/// Does each run's removal in memory, as its task does on `check-removable`'s copy, with
/// `removal`'s for a run of `just keep`, then each part's alone, as `just remove` does, unless a
/// run removes that part and no other, and returns a line for each, with what they found: a fork
/// may remove any one part and keep the rest, `removal` among them, which no run does of a part
/// that every run removes with another, so a line that names it inside the other's block would
/// stay (ARCH-07).
fn removals(repo: &Repo, parts: &[Part], runs: &[Run]) -> (Vec<String>, Vec<Finding>) {
    let read = Read::of(repo, parts);
    let mut report = Vec::new();
    let mut findings = Vec::new();
    let removed: Vec<Run> = runs.iter().map(|run| removed(run, parts)).collect();
    for run in &removed {
        if run.parts.is_empty() {
            report.push(format!(
                "{}: every part it removes is removed already",
                run.name
            ));
            continue;
        }
        let (summary, found) = remove(repo, parts, run, &read);
        report.push(summary);
        findings.extend(found);
    }
    for part in parts {
        if removed.iter().any(|run| run.parts == [part.name.as_str()]) {
            continue;
        }
        let alone = Run {
            name: format!("{} alone", part.name),
            parts: vec![part.name.clone()],
            keeps_removal: true,
        };
        let (summary, found) = remove(repo, parts, &alone, &read);
        report.push(summary);
        findings.extend(found);
    }
    (report, findings)
}

/// `run` with the parts that its task removes on its copy: its own, and `removal`, where `parts`
/// lists it, unless the run's task keeps it. A run that lists no part removes none.
fn removed(run: &Run, parts: &[Part]) -> Run {
    let mut removed = run.parts.clone();
    if !removed.is_empty()
        && !run.keeps_removal
        && parts.iter().any(|part| part.name == REMOVAL)
        && !removed.iter().any(|part| part == REMOVAL)
    {
        removed.push(REMOVAL.to_owned());
    }
    Run {
        name: run.name.clone(),
        parts: removed,
        keeps_removal: run.keeps_removal,
    }
}

/// What the dry run reads once of the repository, for every removal it does: the parts that own
/// each line of each file whose markers read, what each line cites or links to, and what the
/// modules export and the lines that name it.
struct Read<'a> {
    owners: HashMap<&'a str, Vec<Vec<&'a str>>>,
    citations: citations::Index<'a>,
    exports: exports::Exports<'a>,
}

impl<'a> Read<'a> {
    fn of(repo: &'a Repo, parts: &[Part]) -> Self {
        let known: Vec<&str> = parts.iter().map(|part| part.name.as_str()).collect();
        Self {
            owners: repo
                .files()
                .filter_map(|file| {
                    let owners = markers::owners(&file.text, &known).ok()?;
                    Some((file.path.as_str(), owners))
                })
                .collect(),
            citations: citations::Index::read(repo),
            exports: exports::Exports::read(repo),
        }
    }
}

/// What one removal deletes, as the checks of what its kept lines point at read it.
pub(super) struct Removal<'a> {
    /// The files that it deletes whole.
    owned: &'a HashSet<String>,
    /// The parts that it removes.
    removing: &'a [&'a str],
    /// The parts that own each line of each file whose markers read.
    owners: &'a HashMap<&'a str, Vec<Vec<&'a str>>>,
}

impl Removal<'_> {
    /// Whether it deletes the file at `path` whole.
    pub(super) fn deletes_file(&self, path: &str) -> bool {
        self.owned.contains(path)
    }

    /// Whether it deletes the line `number`, counted from 1, of the file at `path`: the file
    /// goes whole, or a part that it removes owns the line.
    pub(super) fn deletes(&self, path: &str, number: usize) -> bool {
        self.deletes_file(path) || self.owned_by_removed(path, number) == Some(true)
    }

    /// Whether it keeps the line `number`, counted from 1, of the file at `path`, outside the
    /// files that it writes again. It keeps no line of a file whose markers don't read, which
    /// `just remove` refuses.
    pub(super) fn keeps(&self, path: &str, number: usize) -> bool {
        !self.deletes_file(path)
            && !is_rewritten(path)
            && self.owned_by_removed(path, number) == Some(false)
    }

    /// The text that it keeps of `file`, if it deletes some of its lines and not the file whole.
    pub(super) fn kept_text(&self, file: &File) -> Option<String> {
        if self.deletes_file(&file.path) {
            return None;
        }
        let owners = self.owners.get(file.path.as_str())?;
        let lines = markers::kept(&file.text, owners, self.removing);
        (lines.len() < owners.len()).then(|| {
            let lines: Vec<&str> = lines.iter().map(|(_, line)| *line).collect();
            lines.join("\n")
        })
    }

    /// Whether a part that it removes owns the line `number` of the file at `path`, if the file's
    /// markers read and it has that line.
    fn owned_by_removed(&self, path: &str, number: usize) -> Option<bool> {
        let parts = self.owners.get(path)?.get(number.checked_sub(1)?)?;
        Some(parts.iter().any(|part| self.removing.contains(part)))
    }
}

/// Removes `run`'s parts from `repo` in memory, and returns what it removed, and a finding for
/// each Rust file whose kept lines don't parse, for each kept path or line, other than those of
/// the files the removal writes again, that holds a word of a part it removes, for each kept
/// line that cites a decision, a behavior or a rule, or links to a file or a heading, that it
/// deletes, for each kept line that names a module's export that only lines it deletes give, and,
/// where it keeps `removal`, for each run that the lines it kept of the list and of CI's
/// `removable` job don't both hold.
fn remove(repo: &Repo, parts: &[Part], run: &Run, read: &Read<'_>) -> (String, Vec<Finding>) {
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
        let Some(owners) = read.owners.get(file.path.as_str()) else {
            continue;
        };
        let lines = markers::kept(&file.text, owners, &removing);
        if lines.len() < owners.len() {
            edited += 1;
            if file.path.ends_with(".rs") {
                findings.extend(unparsed(&file.path, &lines));
            }
        }
        if !is_rewritten(&file.path) {
            kept.push((file.path.as_str(), lines));
        }
    }
    let removal = Removal {
        owned: &owned,
        removing: &removing,
        owners: &read.owners,
    };
    findings.extend(read.citations.dangling(repo, &removal));
    findings.extend(read.exports.dangling(&removal));
    if !removing.contains(&REMOVAL) && repo.has(matrix::WORKFLOW) {
        // What the removal leaves of the runs and of the job that makes them still agrees.
        let text = |path: &str| {
            let lines = kept.iter().find(|(kept, _)| *kept == path);
            lines.map_or_else(String::new, |(_, lines)| {
                let lines: Vec<&str> = lines.iter().map(|(_, line)| *line).collect();
                lines.join("\n")
            })
        };
        for found in matrix::disagreements(&text(LIST), &text(matrix::WORKFLOW)) {
            findings.push(Finding::new(matrix::WORKFLOW, 1, found));
        }
    }
    let paths = repo.paths().filter(|path| !owned.contains(*path));
    findings.extend(leftovers(paths, kept.into_iter(), &removed));
    for finding in &mut findings {
        finding.message = format!("removing {}: {}", removing.join(", "), finding.message);
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
pub(super) fn is_rewritten(path: &str) -> bool {
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
    use super::super::citations::TRACKER;
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

    /// The list of one part, `offers`, which owns the directory `src/shop/` and the spec of
    /// `shop`, whose paths hold no word of the part, and of the run that removes it.
    fn shop_list() -> String {
        list()
            .replace("src/offers.rs", "src/shop/")
            .replace("src/offers_more.rs", "docs/spec/shop.md")
    }

    /// Where `check` finds something in `repo`, each with its message.
    fn found(repo: &Repo) -> Vec<(String, usize, String)> {
        check(repo)
            .findings
            .into_iter()
            .map(|finding| (finding.path, finding.line, finding.message))
            .collect()
    }

    /// The places of `found`.
    fn places(found: &[(String, usize, String)]) -> Vec<(&str, usize)> {
        found
            .iter()
            .map(|(path, line, _)| (path.as_str(), *line))
            .collect()
    }

    #[test]
    fn arch_07_a_kept_line_that_cites_a_behavior_or_a_rule_the_removal_deletes_fails() {
        let marker = format!("<!-- {} offers -->", "part:");
        let end = format!("<!-- end {} offers -->", "part:");
        let spec = format!(
            "| ID | Behavior |\n|---|---|\n| THINGS-001 | Kept. |\n| THINGS-002 | Gone. {marker} |\n"
        );
        let rules = format!("### STYLE-01 Kept\n\n{marker}\n### STYLE-02 Gone\nText.\n{end}\n");
        let doc = format!(
            "As THINGS-002 says.\nAs THINGS-001 and STYLE-01 say.\nAs STYLE-02 says.\n\
             THINGS-002 again. {marker}\nxTHINGS-002, THINGS-0022 and STYLE-020 are others.\n\
             As SHOP-001 says.\nProven by `things_002_` and style_02.\nNot by `things_0022`.\n"
        );
        let code = "// THINGS-002 here.\nfn things_002_is_code() {} // and things_002 a comment\n\
                    #[cfg(test)]\nmod tests {\n    // THINGS-002 there.\n}\n";
        let list = shop_list();
        let repo = Repo::of(&[
            (LIST, &list),
            ("docs/spec/things.md", &spec),
            (
                "docs/spec/shop.md",
                "| ID | Behavior |\n|---|---|\n| SHOP-001 | One. |\n",
            ),
            ("docs/standards/style.md", &rules),
            ("docs/a.md", &doc),
            ("docs/inventory/old.md", "THINGS-002 of the old codebase.\n"),
            ("crates/xtask/src/a.rs", code),
            ("src/shop/a.rs", ""),
        ]);

        let found = found(&repo);

        assert_eq!(
            places(&found),
            [
                ("docs/a.md", 1),
                ("docs/a.md", 3),
                ("docs/a.md", 6),
                ("docs/a.md", 7),
                ("docs/a.md", 7),
                ("crates/xtask/src/a.rs", 1),
                ("crates/xtask/src/a.rs", 2),
            ],
            "{found:?}"
        );
        assert_eq!(
            found[0].2,
            "removing offers: cites THINGS-002, whose row the removal deletes from \
             docs/spec/things.md (ARCH-07)"
        );
        assert_eq!(
            found[1].2,
            "removing offers: cites STYLE-02, whose heading the removal deletes from \
             docs/standards/style.md (ARCH-07)"
        );
    }

    #[test]
    fn arch_07_a_kept_link_to_a_file_or_a_heading_the_removal_deletes_fails() {
        let marker = format!("<!-- {} offers -->", "part:");
        let end = format!("<!-- end {} offers -->", "part:");
        let guide = format!("# Guide\n\n## Kept\n\n{marker}\n## Gone\n\nText.\n{end}\n");
        let doc = format!(
            "# A\n\n[kept](guide.md#kept)\n[gone](guide.md#gone)\n[file](../src/shop/a.rs)\n\
             [directory](../src/shop/)\n[the rest](../src/), [the root](../)\n[missing](guide.md#none)\n\
             [site](https://example.com/#gone)\n[here](#local)\n[gone](guide.md#gone) {marker}\n\n\
             {marker}\n## Local\n{end}\n"
        );
        let code = "/// See [the guide](../docs/guide.md#gone), not [`a`](crate::a) or \
                    `[b](../docs/guide.md#gone)`.\nfn a() {}\n";
        let list = shop_list();
        let repo = Repo::of(&[
            (LIST, &list),
            ("docs/guide.md", &guide),
            ("docs/a.md", &doc),
            ("src/a.rs", code),
            ("src/shop/a.rs", ""),
            ("docs/spec/shop.md", ""),
        ]);

        let found = found(&repo);

        assert_eq!(
            places(&found),
            [
                ("docs/a.md", 4),
                ("docs/a.md", 5),
                ("docs/a.md", 6),
                ("docs/a.md", 10),
                ("src/a.rs", 1),
            ],
            "{found:?}"
        );
        assert_eq!(
            [found[0].2.as_str(), found[1].2.as_str()],
            [
                "removing offers: links to `guide.md#gone`, whose heading the removal deletes \
                 (ARCH-07)",
                "removing offers: links to `../src/shop/a.rs`, which the removal deletes \
                 (ARCH-07)",
            ]
        );
    }

    #[test]
    fn arch_07_a_kept_line_that_names_an_export_that_only_deleted_lines_give_fails() {
        let open = mark("offers");
        let close = end("offers");
        let shop = format!(
            "mod a;\npub(crate) use a::Kept;\n{open}\npub(crate) use a::{{Gone, Both}};\n{close}\n\
             pub(crate) use b::Both;\npub(crate) use a::{{\n    Split,\n{open}\n    Tail,\n{close}\n}};\n"
        );
        let doc = "`shop::Kept` stays.\n`shop::Gone` goes.\n`shop::Both` stays.\n\
                   `other::Gone` stays.\n`shop::Gone_all` and `my_shop::Gone` are others.\n\
                   `shop::Tail` goes.\n`shop::Split` stays.\n";
        let tests = "fn a() {}\n#[cfg(test)]\nmod tests {\n    // shop::Gone\n}\n";
        let list = list();
        let repo = Repo::of(&[
            (LIST, &list),
            ("crates/app/src/modules/shop/mod.rs", &shop),
            (
                "crates/app/src/modules/other/mod.rs",
                "pub(crate) use x::Gone;\n",
            ),
            ("docs/a.md", doc),
            ("src/b.rs", "use crate::modules::shop::Gone;\n"),
            ("docs/MIGRATION.md", "`shop::Gone` goes.\n"),
            ("crates/xtask/src/c.rs", tests),
            ("src/offers.rs", ""),
        ]);

        let found = found(&repo);

        assert_eq!(
            places(&found),
            [("docs/a.md", 2), ("docs/a.md", 6), ("src/b.rs", 1)],
            "{found:?}"
        );
        assert_eq!(
            found[0].2,
            "removing offers: names `shop::Gone`, which only lines of \
             crates/app/src/modules/shop/mod.rs that the removal deletes export (ARCH-07)"
        );
    }

    #[test]
    fn arch_07_a_run_that_a_removal_leaves_in_ci_s_job_and_not_in_the_list_fails() {
        // A run that goes with the part, as its marker says in the list, and whose name
        // holds no word of the part.
        let hash = |marker: String| format!("#{}", &marker[2..]);
        let list = format!(
            "{}  {}\n  - name: stall\n    parts:\n      - offers\n  {}\n",
            list(),
            hash(mark("offers")),
            hash(end("offers"))
        );
        let job = |run: &str| {
            format!(
                "jobs:\n  removable:\n    strategy:\n      matrix:\n        run:\n          \
                 - shop\n{run}"
            )
        };
        let unmarked = job("          - stall\n");
        let marked = job(&format!(
            "          {}\n          - stall\n          {}\n",
            hash(mark("offers")),
            hash(end("offers"))
        ));
        let found = |workflow: &str| -> Vec<String> {
            let repo = Repo::of(&[
                (LIST, &list),
                (matrix::WORKFLOW, workflow),
                ("src/offers.rs", ""),
                ("src/offers_more.rs", ""),
            ]);
            let findings = check(&repo).findings;
            findings.into_iter().map(|found| found.message).collect()
        };

        assert_eq!(found(&marked), Vec::<String>::new());
        assert_eq!(
            found(&unmarked),
            ["removing offers: the job `removable` makes a run `stall`, which the list of \
                 the runs doesn't hold (ARCH-07)"; 2]
        );
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
    fn arch_07_a_line_that_names_a_part_inside_another_s_block_fails_though_no_run_keeps_it() {
        // Every run that removes `offers` removes `coupons` too, whose block holds the line
        // that names `offers`: only the removal of `offers` alone keeps it.
        let hash = |marker: String| format!("#{}", &marker[2..]);
        let part = |name: &str, word: &str| {
            format!(
                "  {}\n  - name: {name}\n    paths:\n      - src/{name}.rs\n    words:\n      - {word}\n  {}\n",
                hash(mark(name)),
                hash(end(name))
            )
        };
        let list = format!(
            "parts:\n{}{}runs:\n  - name: bare\n    parts:\n      - offers {}\n      - coupons {}\n  - name: plain\n    parts:\n      - coupons {}\n",
            part("offers", "offer"),
            part("coupons", "coupon"),
            hash(mark("offers")),
            hash(mark("coupons")),
            hash(mark("coupons")),
        );
        let code = format!(
            "fn a() {{}}\n{}\n// With an offer.\nfn b() {{}}\n{}\n",
            mark("coupons"),
            end("coupons")
        );
        let repo = Repo::of(&[
            (LIST, &list),
            ("src/a.rs", &code),
            ("src/offers.rs", ""),
            ("src/coupons.rs", ""),
        ]);

        let outcome = check(&repo);

        let found: Vec<(&str, usize, &str)> = outcome
            .findings
            .iter()
            .map(|finding| {
                (
                    finding.path.as_str(),
                    finding.line,
                    finding.message.as_str(),
                )
            })
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].0, found[0].1), ("src/a.rs", 3));
        assert!(found[0].2.starts_with("removing offers: "), "{found:?}");
        assert_eq!(
            outcome.report,
            [
                "bare: removing offers, coupons deletes 2 files and edits 2",
                "plain: removing coupons deletes 1 files and edits 2",
                "offers alone: removing offers deletes 1 files and edits 1",
            ]
        );
    }

    /// The list of `removal`, which owns the list and whose word is `remove`, and of `offers`,
    /// with the run `shop`, which removes `offers`, and whatever `more` adds to that run.
    fn list_with_removal(more: &str) -> String {
        let hash = |marker: String| format!("#{}", &marker[2..]);
        format!(
            "parts:\n  - name: removal\n    paths:\n      - {LIST}\n    words:\n      - remove\n  \
             {}\n  - name: offers\n    paths:\n      - src/offers.rs\n    words:\n      - offer\n  \
             {}\nruns:\n  - name: shop\n{more}    parts:\n      - offers {}\n",
            hash(mark("offers")),
            hash(end("offers")),
            hash(mark("offers")),
        )
    }

    /// A file whose block of `removal` holds a line that names `offers`.
    fn names_offers_in_a_block_of_removal() -> String {
        format!(
            "fn a() {{}}\n{}\n// How a fork drops an offer.\nfn b() {{}}\n{}\n",
            mark("removal"),
            end("removal")
        )
    }

    #[test]
    fn arch_07_a_line_that_names_a_part_inside_a_block_of_removal_fails_as_just_remove_would() {
        // The run keeps the other parts, so it removes `removal` with `offers`, and the line
        // goes: only the removal of `offers` alone, which keeps `removal`, leaves it.
        let list = list_with_removal("");
        let code = names_offers_in_a_block_of_removal();
        let repo = Repo::of(&[(LIST, &list), ("src/a.rs", &code), ("src/offers.rs", "")]);

        let outcome = check(&repo);

        let found: Vec<(&str, usize, &str)> = outcome
            .findings
            .iter()
            .map(|finding| {
                (
                    finding.path.as_str(),
                    finding.line,
                    finding.message.as_str(),
                )
            })
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].0, found[0].1), ("src/a.rs", 3));
        assert!(found[0].2.starts_with("removing offers: "), "{found:?}");
        assert_eq!(
            outcome.report,
            [
                "shop: removing removal, offers deletes 2 files and edits 1",
                "removal alone: removing removal deletes 1 files and edits 1",
                "offers alone: removing offers deletes 1 files and edits 1",
            ]
        );
    }

    #[test]
    fn arch_07_a_run_whose_task_is_remove_keeps_removal_as_the_task_does() {
        let list = list_with_removal("    task: remove\n");
        let code = names_offers_in_a_block_of_removal();
        let repo = Repo::of(&[(LIST, &list), ("src/a.rs", &code), ("src/offers.rs", "")]);

        let outcome = check(&repo);

        let found: Vec<(usize, &str)> = outcome
            .findings
            .iter()
            .map(|finding| (finding.line, finding.message.as_str()))
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, 3);
        assert!(found[0].1.starts_with("removing offers: "), "{found:?}");
        assert_eq!(
            outcome.report,
            [
                "shop: removing offers deletes 1 files and edits 1",
                "removal alone: removing removal deletes 1 files and edits 1",
            ]
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
