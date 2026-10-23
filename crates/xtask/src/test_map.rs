//! `check-test-map`: every test's name starts with a behavior or rule ID that exists
//! (TEST-01), and the report shows which behaviors tests prove (DOC-10).
//!
//! While `docs/MIGRATION.md` exists, specs are written ahead of their code, so a behavior that
//! no test proves is reported. Once the migration closes, it fails the check.

use std::collections::BTreeSet;

use syn::{Attribute, ItemFn, visit::Visit};

use crate::{
    Finding, Outcome,
    catalog::{Behavior, Catalog},
    repo::Repo,
};

/// The migration tracker. While it exists, unproven behaviors are reported, not failed.
const TRACKER: &str = "docs/MIGRATION.md";

/// Runs the check.
pub fn check(repo: &Repo, catalog: &Catalog) -> Outcome {
    let mut outcome = Outcome::default();
    let proven = proven(repo, catalog, &mut outcome.findings);
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
                    format!("no test proves {} (DOC-10)", behavior.id),
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
    fn doc_10_the_report_shows_which_behaviors_tests_prove() {
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
    fn doc_10_an_unproven_behavior_fails_once_the_migration_closes() {
        let during = run(&[(TRACKER, "")]);
        let after = run(&[]);

        assert!(during.findings.is_empty());
        let findings: Vec<String> = after.findings.iter().map(ToString::to_string).collect();
        assert_eq!(
            findings,
            [
                "docs/spec/widgets.md:11: no test proves WIDGETS-001 (DOC-10)",
                "docs/spec/widgets.md:12: no test proves WIDGETS-002 (DOC-10)"
            ]
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
