//! `check-docs`, the acronyms: a document spells out each acronym where it first uses it, unless
//! the glossary lists it among those a developer is assumed to know, or among the names written
//! in capitals (DOC-09). The migration workspace isn't checked, as DOC-09's Check line says, and
//! a marker of unfinished work is DOC-07's to report.
//!
//! An acronym counts as spelled out when its first use stands alone in parentheses, as in "the
//! Simple Mail Transfer Protocol (SMTP)", or when it, or the hyphenated name it starts, comes
//! right before them, as in "SMTP (Simple Mail…)" or "SCRAM-SHA-256 (Salted Challenge…)".
//! Whether the words abbreviate the acronym is left to review: an acronym's letters don't always
//! start its words, as OTLP's don't in "OpenTelemetry Protocol", and letters matched inside
//! words would let unrelated text pass.

use std::collections::BTreeSet;

use crate::{Finding, catalog::Catalog, docs, markdown, repo::Repo};

/// The document that holds the glossary.
const GLOSSARY: &str = "docs/architecture.md";

/// How the glossary's sentence that lists the acronyms a developer is assumed to know begins.
const ASSUMED: &str = "The documents assume a developer knows these acronyms";

/// How the glossary's sentence that lists the names written in capitals begins.
const NAMES: &str = "These names are written in capitals";

/// Runs the check.
pub fn check(repo: &Repo, catalog: &Catalog) -> Vec<Finding> {
    let Some(glossary) = repo.file(GLOSSARY) else {
        return Vec::new();
    };
    let mut known = BTreeSet::new();
    let mut findings = Vec::new();
    for opening in [ASSUMED, NAMES] {
        match listed(&glossary.text, opening) {
            Some(words) => known.extend(words),
            None => findings.push(Finding::new(
                GLOSSARY,
                1,
                format!("the glossary has no sentence that starts \"{opening}\" (DOC-09)"),
            )),
        }
    }
    for file in repo.files() {
        if file.path.ends_with(".md") && docs::checks_acronyms(&file.path) {
            findings.extend(first_uses(&file.path, &file.text, &known, catalog));
        }
    }
    findings
}

/// The words a sentence that starts with `opening` lists after its colon, such as
/// "…: API, CI and URL." gives API, CI and URL.
fn listed(text: &str, opening: &str) -> Option<Vec<String>> {
    let start = text.find(opening)?;
    let sentence = &text[start..];
    let after_colon = &sentence[sentence.find(": ")? + 2..];
    let list = &after_colon[..after_colon.find('.')?];
    Some(
        list.split(", ")
            .flat_map(|part| part.split(" and "))
            .map(|word| word.trim().to_owned())
            .filter(|word| !word.is_empty())
            .collect(),
    )
}

/// Each acronym whose first use in the document isn't spelled out.
fn first_uses(path: &str, text: &str, known: &BTreeSet<String>, catalog: &Catalog) -> Vec<Finding> {
    let mut seen = BTreeSet::new();
    let mut findings = Vec::new();
    for (line_number, line) in markdown::outside_code(text) {
        let prose = without_code_and_targets(line);
        for (start, end) in words(&prose) {
            let word = &prose[start..end];
            let acronym = word
                .strip_suffix('s')
                .filter(|stem| is_capitals(stem))
                .unwrap_or(word);
            if !is_capitals(acronym)
                || known.contains(acronym)
                || catalog.knows_prefix(acronym)
                || is_numbered(acronym)
                || is_roman(acronym)
                || docs::UNFINISHED.contains(&acronym)
                || prose[..start].ends_with('/')
                || prose[end..].starts_with(".md")
                || !seen.insert(acronym.to_owned())
            {
                continue;
            }
            if !spelled_out(&prose, start, end) {
                findings.push(Finding::new(
                    path,
                    line_number,
                    format!(
                        "{acronym} isn't spelled out where the document first uses it, and the glossary doesn't list it (DOC-09)"
                    ),
                ));
            }
        }
    }
    findings
}

/// The line with its code spans blanked and its link targets removed, so a word inside them is
/// never read.
fn without_code_and_targets(line: &str) -> String {
    let mut prose = String::with_capacity(line.len());
    let mut in_code = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            in_code = !in_code;
            prose.push(' ');
        } else if !in_code && c == ']' && chars.peek() == Some(&'(') {
            prose.push(']');
            for skipped in chars.by_ref() {
                if skipped == ')' {
                    break;
                }
            }
        } else {
            prose.push(if in_code { ' ' } else { c });
        }
    }
    prose
}

/// The byte ranges of the words of `prose`: runs of ASCII letters and digits.
fn words(prose: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut start = None;
    for (at, c) in prose.char_indices().chain([(prose.len(), ' ')]) {
        match (start, c.is_ascii_alphanumeric()) {
            (None, true) => start = Some(at),
            (Some(from), false) => {
                words.push((from, at));
                start = None;
            }
            _ => {}
        }
    }
    words
}

/// Two or more characters, a capital letter first, and nothing but capitals and digits.
fn is_capitals(word: &str) -> bool {
    word.len() >= 2
        && word.starts_with(|c: char| c.is_ascii_uppercase())
        && word
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// An ID such as D12 or Q1: one capital letter and a number.
fn is_numbered(word: &str) -> bool {
    word.len() >= 2 && word[1..].chars().all(|c| c.is_ascii_digit())
}

/// A Roman numeral, as the factors of the twelve-factor app are numbered.
fn is_roman(word: &str) -> bool {
    word.chars().all(|c| matches!(c, 'I' | 'V' | 'X'))
}

/// Whether the word at `start..end` stands alone in parentheses, or it, or the hyphenated name
/// it starts, comes right before them.
fn spelled_out(prose: &str, start: usize, end: usize) -> bool {
    let alone = prose[..start].ends_with('(') && prose[end..].starts_with(')');
    let name_end = prose[end..].trim_start_matches(|c: char| c == '-' || c.is_ascii_alphanumeric());
    alone || name_end.starts_with(" (")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLOSSARY_TEXT: &str = "## 11. Glossary\n\nThe documents assume a developer knows these acronyms, and don't spell them out: API, HTTP, ID and URL. These names are written in capitals, and aren't acronyms to spell out: SHA and W3C.\n";

    fn findings(document: &str) -> Vec<String> {
        let repo = Repo::of(&[(GLOSSARY, GLOSSARY_TEXT), ("docs/coverage.md", document)]);
        let catalog = Catalog::read(&repo);
        check(&repo, &catalog)
            .into_iter()
            .map(|finding| finding.message)
            .collect()
    }

    #[test]
    fn doc_09_spelled_out_and_listed_acronyms_pass() {
        let document = "Email goes over the Simple Mail Transfer Protocol (SMTP), then SMTP again.\nADRs (architecture decision records), and an HTTP API. SHA-256 is a name, and SCRAM-SHA-256 (Salted Challenge Response Authentication Mechanism) is spelled out. See [the inventory](inventory/SYNTHESIS.md) and `ERROR`. D12 and Q1 are IDs, and so is factor IV.\n";
        assert_eq!(findings(document), Vec::<String>::new());
    }

    #[test]
    fn doc_09_an_acronym_not_spelled_out_where_first_used_fails() {
        let document = "SSE streams events. Server-sent events (SSE) come later.\nIt reads claims (from OIDC providers), then OIDC again.\n";
        assert_eq!(
            findings(document),
            [
                "SSE isn't spelled out where the document first uses it, and the glossary doesn't list it (DOC-09)",
                "OIDC isn't spelled out where the document first uses it, and the glossary doesn't list it (DOC-09)",
            ]
        );
    }
}
