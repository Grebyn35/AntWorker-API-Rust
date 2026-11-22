//! What `check-test-map` reads of the tests' strings against what the modules declare. A string
//! is a string literal, plain or raw, as it is written, without unescaping, at any depth, inside
//! macros too.
//!
//! SEC-19: each workspace event that a module's `events.rs` declares is the whole of a string
//! between the braces of the body of a test whose name starts with `sec_19_`. A test is a
//! function that carries an attribute whose last word is `test`, such as `#[tokio::test]`, so a
//! string of a helper that the test calls isn't read.
//!
//! SEC-18: each action that a module's `audit.rs` declares stands in a string of a Rust file
//! under a crate's `tests/` directory, a helper's there included, with no lowercase letter,
//! digit, `_` or `.` right before or after it, as it does in a query's
//! `action = 'workspace.renamed'`.
//!
//! The declarations are read as `check-openapi` reads them (AUDIT-012, REALTIME-008), and review
//! checks what the tests do with what they name.

use std::collections::BTreeSet;

use proc_macro2::{Delimiter, TokenStream, TokenTree};

use crate::{
    Finding,
    openapi::{actions, events},
    repo::Repo,
    text::line_of,
};

/// The start of the names of the tests that prove SEC-19.
const SEC_19: &str = "sec_19_";

/// A finding for each workspace event that no `sec_19_` test names, and for each audit action
/// that no file of the tests names.
pub(super) fn check(repo: &Repo) -> Vec<Finding> {
    let (mut of_sec_19, mut of_tests) = (BTreeSet::new(), BTreeSet::new());
    for file in repo.files().filter(|file| file.path.ends_with(".rs")) {
        let Ok(tokens) = file.text.parse::<TokenStream>() else {
            continue;
        };
        if is_tests_file(&file.path) {
            all_strings(tokens.clone(), &mut of_tests);
        }
        sec_19_strings(&tokens.into_iter().collect::<Vec<_>>(), &mut of_sec_19);
    }
    let mut findings = Vec::new();
    for file in repo
        .files()
        .filter(|file| events::is_events_file(&file.path))
    {
        for name in events::workspace_events(&file.text) {
            if !of_sec_19.contains(name) {
                findings.push(Finding::new(
                    &file.path,
                    line_of(&file.text, &format!("\"{name}\"")),
                    format!(
                        "no `sec_19_` test names the workspace event `{name}`: test it against a \
                         member with its permission, one without it while some role lacks it, \
                         and one who joined after it (SEC-19)"
                    ),
                ));
            }
        }
    }
    for file in repo
        .files()
        .filter(|file| actions::is_audit_file(&file.path))
    {
        for (name, line) in actions::declared_in(&file.text) {
            if !of_tests.iter().any(|text| holds(text, name)) {
                findings.push(Finding::new(
                    &file.path,
                    line,
                    format!(
                        "no file of the tests names the action `{name}` in a string: have the \
                         test of the behavior that records it read the entry, and assert what \
                         it stores (SEC-18)"
                    ),
                ));
            }
        }
    }
    findings
}

/// Whether `path` is a Rust file under a crate's `tests/` directory.
fn is_tests_file(path: &str) -> bool {
    let inside = path
        .strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'));
    inside.is_some_and(|(_, inside)| inside.starts_with("tests/") && inside.ends_with(".rs"))
}

/// Whether `attribute`, the tokens in an attribute's brackets, marks a test, as the test map
/// reads one: its path's last segment is `test`, with or without arguments after it, such as
/// `#[test]`, `#[tokio::test]` or `#[tokio::test(flavor = "multi_thread")]`.
fn marks_a_test(attribute: &proc_macro2::Group) -> bool {
    let tokens: Vec<TokenTree> = attribute.stream().into_iter().collect();
    let path = match tokens.split_last() {
        Some((TokenTree::Group(_), path)) => path,
        _ => &tokens,
    };
    matches!(path.last(), Some(TokenTree::Ident(ident)) if ident == "test")
}

/// Adds to `strings` each string that the body of a test named `sec_19_*` among `tokens` holds.
fn sec_19_strings(tokens: &[TokenTree], strings: &mut BTreeSet<String>) {
    // Whether the item being read carries a test's attribute, such as `#[tokio::test]`.
    let mut tested = false;
    for (at, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Punct(punct) if punct.as_char() == '#' => {
                if let Some(TokenTree::Group(attribute)) = tokens.get(at + 1)
                    && attribute.delimiter() == Delimiter::Bracket
                    && marks_a_test(attribute)
                {
                    tested = true;
                }
            }
            TokenTree::Punct(punct) if punct.as_char() == ';' => tested = false,
            TokenTree::Ident(ident) if ident == "fn" => {
                let proves = std::mem::take(&mut tested)
                    && matches!(
                        tokens.get(at + 1),
                        Some(TokenTree::Ident(name)) if name.to_string().starts_with(SEC_19)
                    );
                let body = tokens.iter().skip(at + 2).find_map(|token| match token {
                    TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => Some(group),
                    _ => None,
                });
                if let (true, Some(body)) = (proves, body) {
                    all_strings(body.stream(), strings);
                }
            }
            TokenTree::Group(group) => {
                sec_19_strings(&group.stream().into_iter().collect::<Vec<_>>(), strings);
            }
            _ => {}
        }
    }
}

/// Adds to `strings` the text of each string literal among `tokens`, plain or raw, at any
/// depth, as it is written between its quotes, without unescaping it, since neither an event's
/// name nor an action's holds an escape.
fn all_strings(tokens: TokenStream, strings: &mut BTreeSet<String>) {
    for token in tokens {
        match token {
            TokenTree::Literal(literal) => {
                let text = literal.to_string();
                let raw = text.strip_prefix('r').unwrap_or(&text).trim_matches('#');
                if let Some(value) = raw
                    .strip_prefix('"')
                    .and_then(|text| text.strip_suffix('"'))
                {
                    strings.insert(value.to_owned());
                }
            }
            TokenTree::Group(group) => all_strings(group.stream(), strings),
            TokenTree::Ident(_) | TokenTree::Punct(_) => {}
        }
    }
}

/// Whether `text` holds `name` with no lowercase letter, digit, `_` or `.` right before or after
/// it.
fn holds(text: &str, name: &str) -> bool {
    let of_a_name = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.';
    text.match_indices(name).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(of_a_name)
            && !text[at + name.len()..]
                .chars()
                .next()
                .is_some_and(of_a_name)
    })
}

#[cfg(test)]
mod tests {
    use super::super::tests::run_with;

    #[test]
    fn sec_19_a_workspace_event_that_no_sec_19_test_names_fails() {
        let declared = "const A: WorkspaceEvent<P, A> = WorkspaceEvent::new(\"thing.made\");\n\
                        const B: WorkspaceEvent<P, B> =\n    WorkspaceEvent::new(\"thing.gone\");\n\
                        const C: UserEvent<C> = UserEvent::new(\"thing.sent\");\n";
        let tests = "mod things {\n\
                     #[tokio::test]\n\
                     async fn sec_19_thing_events_reach_the_members() {\n\
                     \x20   assert_eq!(event.name(), Some(\"thing.made\"));\n\
                     }\n\
                     #[tokio::test]\n\
                     async fn widgets_001_names_the_other() {\n\
                     \x20   let _ = \"thing.gone\";\n\
                     }\n\
                     fn sec_19_helper() -> &'static str {\n\
                     \x20   \"thing.gone\"\n\
                     }\n\
                     }\n";
        let outcome = run_with(&[
            ("crates/app/src/modules/things/events.rs", declared),
            ("crates/app/tests/integration/things.rs", tests),
        ]);

        let findings: Vec<String> = outcome
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|finding| finding.ends_with("(SEC-19)"))
            .collect();
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings[0].starts_with(
                "crates/app/src/modules/things/events.rs:3: no `sec_19_` test names the \
                 workspace event `thing.gone`"
            ),
            "{findings:?}"
        );
    }

    #[test]
    fn sec_19_a_test_whose_attribute_takes_arguments_names_its_events_too() {
        let declared = "const A: WorkspaceEvent<P, A> = WorkspaceEvent::new(\"thing.made\");\n";
        let tests = "#[tokio::test(flavor = \"multi_thread\")]\n\
                     async fn sec_19_thing_events_reach_the_members() {\n\
                     \x20   assert_eq!(event.name(), Some(\"thing.made\"));\n\
                     }\n";

        let outcome = run_with(&[
            ("crates/app/src/modules/things/events.rs", declared),
            ("crates/app/tests/integration/things.rs", tests),
        ]);

        let findings: Vec<String> = outcome
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|finding| finding.ends_with("(SEC-19)"))
            .collect();
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn sec_18_an_action_that_no_test_names_in_a_string_fails() {
        let declared = "const A: Action<NoDetails> = Action::new(\"thing.made\");\n\
                        const B: Action<NoDetails> =\n    Action::new(\"thing.gone\");\n\
                        const C: Action<NoDetails> = Action::new(\"thing.sent\");\n\
                        const D: Action<NoDetails> = Action::new(\"thing.kept\");\n";
        // In the tests' files, a helper's query names the first, and a raw string the fourth;
        // the third stands only inside longer names, and the second only in a file of `src/`.
        let tests = "fn entries() {\n\
                     \x20   reads(\"SELECT 1 FROM audit_entries WHERE action = 'thing.made'\");\n\
                     }\n\
                     #[tokio::test]\n\
                     async fn things_001_records_what_it_made() {\n\
                     \x20   assert_eq!(entry[r#\"action\"#], r#\"thing.kept\"#);\n\
                     \x20   let _ = (\"thing.sent_at\", \"a_thing.sent\", \"thing.sent.twice\");\n\
                     }\n";
        let unit = "#[test]\nfn things_001_names_it() {\n    let _ = \"thing.gone\";\n}\n";
        let outcome = run_with(&[
            ("crates/app/src/modules/things/audit.rs", declared),
            ("crates/app/tests/integration/things.rs", tests),
            ("crates/app/src/modules/things/service.rs", unit),
        ]);

        let findings: Vec<String> = outcome
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|finding| finding.ends_with("(SEC-18)"))
            .collect();
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(
            findings[0].starts_with(
                "crates/app/src/modules/things/audit.rs:3: no file of the tests names the action \
                 `thing.gone`"
            ),
            "{findings:?}"
        );
        assert!(
            findings[1].starts_with(
                "crates/app/src/modules/things/audit.rs:4: no file of the tests names the action \
                 `thing.sent`"
            ),
            "{findings:?}"
        );
    }
}
