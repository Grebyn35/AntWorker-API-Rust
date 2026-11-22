//! What the lines that a removal keeps cite and link to, read once for every removal that
//! `check-parts` does in memory: the removal deletes every link and ID that points into what it
//! removes (ARCH-07), so a kept line that cites a decision, a behavior or a rule whose row or
//! heading it deletes, or that links to a file or a heading that it deletes, fails.

use std::collections::{BTreeSet, HashMap};

use super::dry_run::{Removal, is_rewritten};
use crate::{Finding, catalog, docs, markdown, references, repo::Repo, text};

/// The tracker, whose table of decisions gives each decision a row that starts `| D<number> |`.
pub(super) const TRACKER: &str = "docs/MIGRATION.md";

/// The inventory, whose IDs are its own (DOC-12).
const INVENTORY: &str = "docs/inventory/";

/// What a line that defines an ID is.
#[derive(Clone, Copy)]
enum Definition {
    /// A decision's row in the tracker.
    Decision,
    /// A behavior's row in a spec.
    Behavior,
    /// A rule's heading in the standards.
    Rule,
}

/// A link to what a file of the repository holds, with the line that holds it.
struct Link<'a> {
    path: &'a str,
    line: usize,
    destination: String,
    target: String,
    anchor: String,
}

/// What every line of the repository, outside the files a removal writes again, cites, links to
/// and defines.
pub(super) struct Index<'a> {
    /// Each ID that a line defines: its file, its line, the ID, and what the line is.
    defined: Vec<(&'a str, usize, String, Definition)>,
    /// Each ID that a line cites: its file, its line, and the ID.
    cited: Vec<(&'a str, usize, String)>,
    /// Each link to a file, or a directory, that the repository holds.
    links: Vec<Link<'a>>,
    /// The anchors of each document's headings.
    anchors: HashMap<&'a str, BTreeSet<String>>,
}

impl<'a> Index<'a> {
    /// Reads `repo`: the decisions that the tracker's rows define, the behaviors that the specs'
    /// rows define, and the rules that the headings of the standards define, as `check-docs`
    /// reads the last two (DOC-12); the decisions that each line cites, `D` and its number, with no
    /// ASCII letter or digit right before or after, so `D12` cites no `D1`; the behaviors and the
    /// rules that each line cites, outside the inventory and the tests of the checks, as
    /// `check-docs` reads them: an ID-shaped token that stands as a word of its own, and the test
    /// prefix, such as `workspaces_005`, of a document's line or of a comment; and the links of
    /// each document, and of each Rust file's doc comments, that `check-docs` resolves.
    pub(super) fn read(repo: &'a Repo) -> Self {
        let mut index = Self {
            defined: Vec::new(),
            cited: Vec::new(),
            links: Vec::new(),
            anchors: HashMap::new(),
        };
        for file in repo.files().filter(|file| !is_rewritten(&file.path)) {
            let path = file.path.as_str();
            let rules = catalog::document_in(path, "docs/standards/").is_some();
            let spec = catalog::document_in(path, "docs/spec/").is_some();
            let citing = if path.starts_with(INVENTORY) {
                0
            } else {
                text::citing_lines(path, &file.text)
            };
            for (at, line) in file.text.lines().enumerate() {
                let number = at + 1;
                let defined = if path == TRACKER {
                    decision(line).map(|id| (id.to_owned(), Definition::Decision))
                } else if rules {
                    catalog::rule_heading(line).map(|token| (token.id(), Definition::Rule))
                } else if spec {
                    catalog::behavior_row(line).map(|token| (token.id(), Definition::Behavior))
                } else {
                    None
                };
                if let Some((id, definition)) = defined {
                    index.defined.push((path, number, id, definition));
                }
                for id in decisions(line) {
                    index.cited.push((path, number, id.to_owned()));
                }
                if at < citing {
                    for token in text::id_tokens(line) {
                        index.cited.push((path, number, token.id()));
                    }
                    let described = if path.ends_with(".md") {
                        Some(line)
                    } else {
                        text::comment(path, line)
                    };
                    for (prefix, digits) in
                        described.map(references::test_prefixes).unwrap_or_default()
                    {
                        let id = format!("{}-{digits}", prefix.to_ascii_uppercase());
                        index.cited.push((path, number, id));
                    }
                    if path.ends_with(".rs") {
                        for destination in docs::comment_link_destinations(line) {
                            index.link(repo, path, number, destination);
                        }
                    }
                }
            }
            if path.ends_with(".md") {
                let document = markdown::read(&file.text);
                for link in document.links {
                    index.link(repo, path, link.line, link.destination);
                }
                index.anchors.insert(path, document.anchors);
            }
        }
        index
    }

    /// Adds the link to `destination` at the line `number` of the file at `path`, if it points at
    /// what the repository holds.
    fn link(&mut self, repo: &Repo, path: &'a str, number: usize, destination: String) {
        let Some((target, anchor)) = docs::link_target(path, &destination) else {
            return;
        };
        let anchor = anchor.to_owned();
        if repo.has(&target) {
            self.links.push(Link {
                path,
                line: number,
                destination,
                target,
                anchor,
            });
        }
    }

    /// A finding for each line that `removal` keeps and that cites an ID whose defining line it
    /// deletes, or links to a file or a directory that it deletes whole, or to an anchor of a
    /// document that it edits and whose kept text has no heading with that anchor.
    pub(super) fn dangling(&self, repo: &Repo, removal: &Removal<'_>) -> Vec<Finding> {
        let deleted: HashMap<&str, (&str, Definition)> = self
            .defined
            .iter()
            .filter(|(path, number, _, _)| removal.deletes(path, *number))
            .map(|(path, _, id, definition)| (id.as_str(), (*path, *definition)))
            .collect();
        let mut findings = Vec::new();
        for (path, number, id) in &self.cited {
            let Some((from, definition)) = deleted.get(id.as_str()) else {
                continue;
            };
            if !removal.keeps(path, *number) {
                continue;
            }
            let message = match definition {
                Definition::Decision => {
                    format!("cites {id}, whose decision the removal deletes from the tracker")
                }
                Definition::Behavior => {
                    format!("cites {id}, whose row the removal deletes from {from}")
                }
                Definition::Rule => {
                    format!("cites {id}, whose heading the removal deletes from {from}")
                }
            };
            findings.push(Finding::new(path, *number, format!("{message} (ARCH-07)")));
        }
        let mut gone: HashMap<&str, bool> = HashMap::new();
        let mut kept: HashMap<&str, Option<BTreeSet<String>>> = HashMap::new();
        for link in self
            .links
            .iter()
            .filter(|link| removal.keeps(link.path, link.line))
        {
            let target = link.target.as_str();
            let goes = *gone
                .entry(target)
                .or_insert_with(|| deletes_whole(repo, removal, target));
            let message = if goes {
                "which the removal deletes"
            } else if link.anchor.is_empty()
                || !self
                    .anchors
                    .get(target)
                    .is_some_and(|anchors| anchors.contains(&link.anchor))
            {
                continue;
            } else {
                let anchors = kept.entry(target).or_insert_with(|| {
                    let file = repo.file(target)?;
                    Some(markdown::read(&removal.kept_text(file)?).anchors)
                });
                if anchors
                    .as_ref()
                    .is_none_or(|anchors| anchors.contains(&link.anchor))
                {
                    continue;
                }
                "whose heading the removal deletes"
            };
            findings.push(Finding::new(
                link.path,
                link.line,
                format!("links to `{}`, {message} (ARCH-07)", link.destination),
            ));
        }
        findings
    }
}

/// Whether `removal` deletes the file at `target`, or every file under the directory there, which
/// the repository's root, the empty path, isn't.
fn deletes_whole(repo: &Repo, removal: &Removal<'_>, target: &str) -> bool {
    if repo.file(target).is_some() {
        return removal.deletes_file(target);
    }
    let directory = format!("{}/", target.trim_end_matches('/'));
    let mut under = repo
        .paths()
        .filter(|path| path.starts_with(&directory))
        .peekable();
    under.peek().is_some() && under.all(|path| removal.deletes_file(path))
}

/// The decision ID whose row in the tracker `line` is, such as `D7` in `| D7 | ...`.
fn decision(line: &str) -> Option<&str> {
    let text = line.strip_prefix("| ")?;
    let digits = text
        .strip_prefix('D')?
        .find(|c: char| !c.is_ascii_digit())
        .filter(|length| *length > 0)?;
    text[1 + digits..]
        .starts_with(" |")
        .then(|| &text[..=digits])
}

/// The decisions that `line` cites: each `D` and its number with no ASCII letter or digit right
/// before or after them, so `D12` cites no `D1`, and `XD2` none.
fn decisions(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    line.match_indices('D')
        .filter_map(|(at, _)| {
            let digits = bytes[at + 1..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            let end = at + 1 + digits;
            let alone = !(at > 0 && bytes[at - 1].is_ascii_alphanumeric())
                && !bytes.get(end).is_some_and(u8::is_ascii_alphanumeric);
            (digits > 0 && alone).then(|| &line[at..end])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_07_a_line_cites_a_decision_as_a_word_of_its_own() {
        assert_eq!(
            decisions("As D2 says, and D20 (D3); not XD2, D2x or D."),
            ["D2", "D20", "D3"]
        );
        // Only an ASCII letter or digit beside it makes it no citation.
        assert_eq!(decisions("éD4, D5é and D6_"), ["D4", "D5", "D6"]);
        assert_eq!(decision("| D7 | A decision. |"), Some("D7"));
        assert_eq!(decision("| D | No number. |"), None);
        assert_eq!(decision("| D7x | Another. |"), None);
    }
}
