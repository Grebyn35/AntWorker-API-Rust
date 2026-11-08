//! `check-test-map`: every test's name starts with a behavior or rule ID that exists
//! (TEST-01), each workspace event a module declares is named in a `sec_19_` test (SEC-19), and
//! the report shows which behaviors tests prove (DOC-09).
//!
//! While `docs/MIGRATION.md` exists, specs are written ahead of their code, so a behavior that
//! no test proves is reported. Once the migration closes, it fails the check.

use std::collections::BTreeSet;

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::{Attribute, ItemFn, visit::Visit};

use crate::{
    Finding, Outcome,
    catalog::{Behavior, Catalog},
    openapi::events,
    repo::Repo,
    text::line_of,
};

/// The migration tracker. While it exists, unproven behaviors are reported, not failed.
const TRACKER: &str = "docs/MIGRATION.md";

/// The start of the names of the tests that prove SEC-19.
const SEC_19: &str = "sec_19_";

/// Runs the check.
pub fn check(repo: &Repo, catalog: &Catalog) -> Outcome {
    let mut outcome = Outcome::default();
    let proven = proven(repo, catalog, &mut outcome.findings);
    outcome.findings.extend(untested_events(repo));
    let strict = repo.file(TRACKER).is_none();
    let (mut proven_count, mut total) = (0, 0);
    for (prefix, behaviors) in catalog.behaviors() {
        let unproven: Vec<&Behavior> = behaviors
            .iter()
            .filter(|behavior| !proven.contains(&behavior.id))
            .collect();
        proven_count += behaviors.len() - unproven.len();
        total += behaviors.len();
        outcome
            .report
            .push(module_line(prefix, behaviors.len(), &unproven));
        if strict {
            outcome.findings.extend(unproven.iter().map(|behavior| {
                Finding::new(
                    &behavior.path,
                    behavior.line,
                    format!("no test proves {} (DOC-09)", behavior.id),
                )
            }));
        }
    }
    outcome.report.insert(
        0,
        format!(
            "check-test-map: tests prove {proven_count} of the {total} behaviors in the specs."
        ),
    );
    outcome
}

/// The IDs that tests name. A test whose name starts with no ID, or with an ID that doesn't
/// exist, is reported.
fn proven(repo: &Repo, catalog: &Catalog, findings: &mut Vec<Finding>) -> BTreeSet<String> {
    let mut proven = BTreeSet::new();
    for (path, line, name) in tests(repo, findings) {
        let problem = match id_of(&name) {
            None => "doesn't start with a behavior or rule ID".to_owned(),
            Some(id) if !catalog.defines(&id) => {
                format!("names {id}, which no spec or rule defines")
            }
            Some(id) => {
                proven.insert(id);
                continue;
            }
        };
        findings.push(Finding::new(
            &path,
            line,
            format!("test `{name}` {problem} (TEST-01)"),
        ));
    }
    proven
}

/// SEC-19: a finding for each workspace event that a module's `events.rs` declares and no
/// `sec_19_` test names as a string, so that each is tested against a member who joined after
/// it. Review checks what the test does with it.
fn untested_events(repo: &Repo) -> Vec<Finding> {
    let mut named = BTreeSet::new();
    for file in repo.files().filter(|file| file.path.ends_with(".rs")) {
        if let Ok(tokens) = file.text.parse::<TokenStream>() {
            sec_19_strings(&tokens.into_iter().collect::<Vec<_>>(), &mut named);
        }
    }
    let mut findings = Vec::new();
    for file in repo
        .files()
        .filter(|file| events::is_events_file(&file.path))
    {
        for name in events::workspace_events(&file.text) {
            if !named.contains(name) {
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
    findings
}

/// Adds to `strings` each string that the body of a function named `sec_19_*` among `tokens`
/// holds, at any depth, inside macros too.
fn sec_19_strings(tokens: &[TokenTree], strings: &mut BTreeSet<String>) {
    for (at, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(ident) if ident == "fn" => {
                let proves = matches!(
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

/// Adds to `strings` the value of each string literal among `tokens`, at any depth, without
/// unescaping it, since an event's name holds no escape.
fn all_strings(tokens: TokenStream, strings: &mut BTreeSet<String>) {
    for token in tokens {
        match token {
            TokenTree::Literal(literal) => {
                let text = literal.to_string();
                if let Some(value) = text
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

/// One module's line of the report: how many of its behaviors tests prove, and, once some are,
/// which ones they don't.
fn module_line(prefix: &str, total: usize, unproven: &[&Behavior]) -> String {
    let counts = format!("{} of {total}", total - unproven.len());
    let mut line = format!("  {prefix:<18}{counts:>9}");
    if !unproven.is_empty() && unproven.len() < total {
        let ids: Vec<&str> = unproven
            .iter()
            .map(|behavior| behavior.id.as_str())
            .collect();
        line.push_str(&format!("  unproven: {}", ids.join(", ")));
    }
    line
}

/// Every test function in the repository's Rust files, as its path, line and name. A file that
/// doesn't parse is reported.
fn tests(repo: &Repo, findings: &mut Vec<Finding>) -> Vec<(String, usize, String)> {
    let mut tests = Vec::new();
    for file in repo.files().filter(|file| file.path.ends_with(".rs")) {
        let syntax = match syn::parse_file(&file.text) {
            Ok(syntax) => syntax,
            Err(error) => {
                findings.push(Finding::new(
                    &file.path,
                    1,
                    format!("can't read the file as Rust: {error}"),
                ));
                continue;
            }
        };
        let mut names = TestNames::default();
        names.visit_file(&syntax);
        tests.extend(names.0.into_iter().map(|name| {
            let line = line_of_fn(&file.text, &name);
            (file.path.clone(), line, name)
        }));
    }
    tests
}

/// The names of the test functions in a file, in order.
#[derive(Default)]
struct TestNames(Vec<String>);

impl<'ast> Visit<'ast> for TestNames {
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if item.attrs.iter().any(is_test) {
            self.0.push(item.sig.ident.to_string());
        }
        syn::visit::visit_item_fn(self, item);
    }
}

/// Whether `attribute` marks a test, such as `#[test]` or `#[tokio::test]`.
fn is_test(attribute: &Attribute) -> bool {
    attribute
        .path()
        .segments
        .last()
        .is_some_and(|segment| segment.ident == "test")
}

/// The line, counted from 1, that defines the function `name` in `text`.
fn line_of_fn(text: &str, name: &str) -> usize {
    text.find(&format!("fn {name}("))
        .map_or(1, |offset| text[..offset].matches('\n').count() + 1)
}

/// The ID a test's name starts with: the words before its first number, in capitals, and that
/// number of two or three digits. `auth_001_signs_in` starts with `AUTH-001`.
fn id_of(name: &str) -> Option<String> {
    let words: Vec<&str> = name.split('_').collect();
    let position = words
        .iter()
        .position(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit()))?;
    let prefix = &words[..position];
    let number = words[position];
    let shaped = !prefix.is_empty()
        && prefix.iter().all(|word| !word.is_empty())
        && matches!(number.len(), 2 | 3);
    shaped.then(|| format!("{}-{number}", prefix.join("_").to_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::fixture;

    const TESTS: &str = "#[cfg(test)]
mod tests {
    #[test]
    fn widgets_001_works() {}

    #[tokio::test]
    async fn style_01_holds() {}

    #[test]
    fn unnamed() {}

    #[test]
    fn widgets_009_is_not_specified() {}

    fn helper() {}
}
";

    fn run(extra: &[(&'static str, &'static str)]) -> Outcome {
        let mut files = fixture();
        files.extend_from_slice(extra);
        let repo = Repo::of(&files);
        check(&repo, &Catalog::read(&repo))
    }

    #[test]
    fn test_01_a_test_must_start_with_an_id_that_exists() {
        let outcome = run(&[("crates/widgets/src/lib.rs", TESTS), (TRACKER, "")]);

        let findings: Vec<String> = outcome.findings.iter().map(ToString::to_string).collect();
        assert_eq!(
            findings,
            [
                "crates/widgets/src/lib.rs:10: test `unnamed` doesn't start with a behavior or rule ID (TEST-01)",
                "crates/widgets/src/lib.rs:13: test `widgets_009_is_not_specified` names WIDGETS-009, which no spec or rule defines (TEST-01)"
            ]
        );
    }

    #[test]
    fn doc_09_the_report_shows_which_behaviors_tests_prove() {
        let outcome = run(&[("crates/widgets/src/lib.rs", TESTS), (TRACKER, "")]);

        let widgets = format!("  WIDGETS{}1 of 2  unproven: WIDGETS-002", " ".repeat(14));
        assert_eq!(
            outcome.report,
            [
                "check-test-map: tests prove 1 of the 2 behaviors in the specs.",
                widgets.as_str()
            ]
        );
    }

    #[test]
    fn doc_09_an_unproven_behavior_fails_once_the_migration_closes() {
        let during = run(&[(TRACKER, "")]);
        let after = run(&[]);

        assert!(during.findings.is_empty());
        let findings: Vec<String> = after.findings.iter().map(ToString::to_string).collect();
        assert_eq!(
            findings,
            [
                "docs/spec/widgets.md:11: no test proves WIDGETS-001 (DOC-09)",
                "docs/spec/widgets.md:12: no test proves WIDGETS-002 (DOC-09)"
            ]
        );
    }

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
                     }\n";
        let outcome = run(&[
            ("crates/app/src/modules/things/events.rs", declared),
            ("crates/app/tests/integration/things.rs", tests),
            (TRACKER, ""),
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
    fn test_01_an_id_is_read_from_the_start_of_a_name() {
        assert_eq!(id_of("auth_001_signs_in").as_deref(), Some("AUTH-001"));
        assert_eq!(id_of("ops_03_one_shape").as_deref(), Some("OPS-03"));
        assert_eq!(
            id_of("gadget_parts_004_x").as_deref(),
            Some("GADGET_PARTS-004")
        );
        assert_eq!(id_of("platform_9_x"), None);
        assert_eq!(id_of("works"), None);
        assert_eq!(id_of("_001_x"), None);
    }
}
