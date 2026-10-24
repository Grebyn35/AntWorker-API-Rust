//! The IDs the documentation defines, and the CI checks the rules name.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Finding,
    repo::{File, Repo},
    text,
};

/// A behavior a module's spec defines, with where it is defined.
#[derive(Debug, PartialEq, Eq)]
pub struct Behavior {
    /// Its ID, such as `AUTH-001`.
    pub id: String,
    /// The spec that defines it.
    pub path: String,
    /// The line of its row in the spec.
    pub line: usize,
}

/// A CI check that a rule names as `ci: <check>`, with where the rule names it.
#[derive(Debug, PartialEq, Eq)]
pub struct NamedCheck {
    /// The check's name, such as `check-docs`.
    pub name: String,
    /// The standards file that names it.
    pub path: String,
    /// The line that names it.
    pub line: usize,
}

/// The IDs the documentation defines: rule IDs in the headings of `docs/standards/`, and
/// behavior IDs in the first column of the tables in `docs/spec/`.
#[derive(Debug, Default)]
pub struct Catalog {
    defined: BTreeSet<String>,
    prefixes: BTreeSet<String>,
    behaviors: BTreeMap<String, Vec<Behavior>>,
    named_checks: Vec<NamedCheck>,
    findings: Vec<Finding>,
}

impl Catalog {
    /// The IDs that `repo` defines. Module prefixes come from the specs and from the module
    /// table in `docs/architecture.md`, so an ID of a module without a spec is still read as an
    /// ID.
    pub fn read(repo: &Repo) -> Self {
        let mut catalog = Self::default();
        for file in repo.files() {
            if document_in(&file.path, "docs/standards/").is_some() {
                catalog.read_rules(file);
            } else if let Some(module) = document_in(&file.path, "docs/spec/") {
                catalog.read_behaviors(file, module);
            }
        }
        if let Some(architecture) = repo.file("docs/architecture.md") {
            catalog.prefixes.extend(modules(&architecture.text));
        }
        catalog
    }

    /// Whether `prefix` starts the IDs of rules, or of a module's behaviors.
    pub fn knows_prefix(&self, prefix: &str) -> bool {
        self.prefixes.contains(prefix)
    }

    /// Whether the documentation defines `id`.
    pub fn defines(&self, id: &str) -> bool {
        self.defined.contains(id)
    }

    /// Each module's behaviors, by ID prefix, in the order its spec lists them.
    pub fn behaviors(&self) -> &BTreeMap<String, Vec<Behavior>> {
        &self.behaviors
    }

    /// The CI checks the rules name.
    pub fn named_checks(&self) -> &[NamedCheck] {
        &self.named_checks
    }

    /// Problems in how the IDs are defined: an ID defined twice, or a behavior in the spec of
    /// another module.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    fn read_rules(&mut self, file: &File) {
        for (index, line) in file.text.lines().enumerate() {
            if let Some(heading) = line.strip_prefix("### ")
                && let Some(token) = text::id_tokens(heading).into_iter().next()
                && token.column == 0
                && token.number.len() == 2
            {
                self.define(token.prefix, &token.id(), &file.path, index + 1);
            }
            for (start, _) in line.match_indices("ci: ") {
                let name: String = line[start + 4..]
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
                    .collect();
                if !name.is_empty() {
                    self.named_checks.push(NamedCheck {
                        name,
                        path: file.path.clone(),
                        line: index + 1,
                    });
                }
            }
        }
    }

    fn read_behaviors(&mut self, file: &File, module: &str) {
        let expected = module.to_uppercase();
        self.prefixes.insert(expected.clone());
        for (index, line) in file.text.lines().enumerate() {
            let Some(cell) = line.strip_prefix('|').and_then(|row| row.split('|').next()) else {
                continue;
            };
            let cell = cell.trim();
            let Some(token) = text::id_tokens(cell).into_iter().next() else {
                continue;
            };
            let id = token.id();
            if token.column != 0 || id.len() != cell.len() || token.number.len() != 3 {
                continue;
            }
            if token.prefix != expected {
                self.findings.push(Finding::new(
                    &file.path,
                    index + 1,
                    format!("{id} is in the spec of `{module}`, whose IDs start with {expected}- (DOC-15)"),
                ));
            }
            if self.define(token.prefix, &id, &file.path, index + 1) {
                self.behaviors
                    .entry(token.prefix.to_owned())
                    .or_default()
                    .push(Behavior {
                        id,
                        path: file.path.clone(),
                        line: index + 1,
                    });
            }
        }
    }

    /// Records `id` as defined at `line` of `path`, and reports whether it was new.
    fn define(&mut self, prefix: &str, id: &str, path: &str, line: usize) -> bool {
        self.prefixes.insert(prefix.to_owned());
        let new = self.defined.insert(id.to_owned());
        if !new {
            self.findings.push(Finding::new(
                path,
                line,
                format!("{id} is defined more than once (DOC-13)"),
            ));
        }
        new
    }
}

/// The name of a Markdown file directly inside `directory`, other than its README.
pub fn document_in<'a>(path: &'a str, directory: &str) -> Option<&'a str> {
    path.strip_prefix(directory)?
        .strip_suffix(".md")
        .filter(|name| *name != "README" && !name.contains('/'))
}

/// The module names in the table under "What each module owns" in `docs/architecture.md`, in
/// capitals: the prefixes of their behavior IDs.
fn modules(architecture: &str) -> Vec<String> {
    architecture
        .lines()
        .skip_while(|line| *line != "### What each module owns")
        .skip(1)
        .take_while(|line| !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix("| `")?.split('`').next())
        .filter(|name| {
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
        .map(str::to_uppercase)
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A repository whose documents keep their formats, with two rule files, one spec and the
    /// module table, for the checks' tests.
    pub fn fixture() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "docs/standards/README.md",
                "# Standards\n\n## Files\n\n| Prefix | File |\n|---|---|\n\
                 | `STYLE` | [style.md](style.md) |\n| `DOC` | [docs.md](docs.md) |\n\n\
                 ## Rule format\n\n| Check | Meaning |\n|---|---|\n\
                 | `compiler` | It doesn't compile |\n| `ci: <task>` | A task fails |\n\
                 | `review` | A reviewer checks it |\n",
            ),
            (
                "docs/standards/style.md",
                "# Style\n\n### STYLE-01 A rule\nThe statement.\n- Why: a reason.\n- Check: `ci: check-style`.\n",
            ),
            (
                "docs/standards/docs.md",
                "# Docs\n\n### DOC-16 The tracker\nIts sections:\n\
                 1. **Current work.** The next step.\n2. **Decisions.** The index.\n\
                 - Why: a reason.\n- Check: `review`.\n",
            ),
            (
                "docs/spec/README.md",
                "# Specs\n\n## Sections\n\n1. **Purpose.** Why it exists.\n\
                 2. **Routes.** Only in a spec whose module serves routes.\n\
                 3. **Behaviors.** What it guarantees.\n\n\
                 ## Specs\n\n| Module | Spec |\n|---|---|\n| `widgets` | [widgets.md](widgets.md) |\n",
            ),
            (
                "docs/spec/widgets.md",
                "# Spec\n\n## Purpose\n\nWidgets.\n\n## Behaviors\n\n\
                 | ID | Behavior | Rules | Test prefix |\n|---|---|---|---|\n\
                 | WIDGETS-001 | One | STYLE-01 | `widgets_001_` |\n\
                 | WIDGETS-002 | Two | STYLE-01 | `widgets_002_` |\n",
            ),
            (
                "docs/architecture.md",
                "### What each module owns\n\n| Module | Owns |\n|---|---|\n| `widgets` | widgets |\n| `gadgets` | none |\n\n## Glossary\n\nThe documents assume a developer knows these acronyms: CI and ID. These names are written in capitals, and aren't acronyms to spell out: SHA.\n",
            ),
        ]
    }

    /// The fixture, with each of `changes` replacing the file at its path, or added.
    pub fn fixture_with(
        changes: &[(&'static str, &'static str)],
    ) -> Vec<(&'static str, &'static str)> {
        let mut files = fixture();
        for change in changes {
            match files.iter_mut().find(|file| file.0 == change.0) {
                Some(file) => *file = *change,
                None => files.push(*change),
            }
        }
        files
    }

    #[test]
    fn doc_13_ids_come_from_rule_headings_and_spec_tables() {
        let catalog = Catalog::read(&Repo::of(&fixture()));

        assert!(catalog.defines("STYLE-01"));
        assert!(catalog.defines("WIDGETS-002"));
        assert!(!catalog.defines("WIDGETS-003"));
        assert!(catalog.knows_prefix("GADGETS"));
        assert!(!catalog.knows_prefix("SHA"));
        let ids: Vec<&str> = catalog.behaviors()["WIDGETS"]
            .iter()
            .map(|behavior| behavior.id.as_str())
            .collect();
        assert_eq!(ids, ["WIDGETS-001", "WIDGETS-002"]);
        assert_eq!(catalog.named_checks()[0].name, "check-style");
        assert!(catalog.findings().is_empty());
    }

    /// The catalog's findings with `spec` as the `gadgets` module's spec.
    fn messages(spec: &'static str) -> Vec<String> {
        let catalog = Catalog::read(&Repo::of(&fixture_with(&[("docs/spec/gadgets.md", spec)])));
        catalog
            .findings()
            .iter()
            .map(|finding| finding.message.clone())
            .collect()
    }

    #[test]
    fn doc_13_an_id_defined_twice_is_reported() {
        assert_eq!(
            messages("| GADGETS-001 | One |\n| GADGETS-001 | Again |\n"),
            ["GADGETS-001 is defined more than once (DOC-13)"]
        );
    }

    #[test]
    fn doc_15_a_behavior_in_another_modules_spec_is_reported() {
        assert_eq!(
            messages("| WIDGETS-009 | Misplaced |\n"),
            ["WIDGETS-009 is in the spec of `gadgets`, whose IDs start with GADGETS- (DOC-15)"]
        );
    }
}
