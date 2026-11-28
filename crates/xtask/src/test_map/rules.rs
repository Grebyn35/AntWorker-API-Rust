//! TEST-01: what a rule's `Check` line gives to `test`, a test proves. The check reads each rule
//! of `docs/standards/`, a heading `### <ID>` with the line that starts `- Check:` before the
//! next heading, and fails on a rule whose line holds `` `test` `` unless a test's name starts
//! with the rule's ID, or the line cites the ID of a behavior or of another rule that a test's
//! name starts with. Of the lines that two spaces indent right after the `Check` line, the list
//! nested under it, the check reads no ID, and fails on each that holds `` `test` `` where the
//! `Check` line holds none. It also fails on each test whose name starts with the ID of a rule
//! whose `Check` line holds no `` `test` ``, but one whose path starts with `crates/xtask/`, a
//! check's own test, or with `crates/app/tests/e2e/`, an end-to-end test: the line's `ci:` items
//! stand for those. Which tests prove the rule's words stays review's.

use super::Named;
use crate::{Finding, catalog::document_in, repo::Repo, text};

/// How a `Check` line names a test as what enforces its rule.
const TEST: &str = "`test`";

/// The start of a rule's `Check` line.
const CHECK: &str = "- Check:";

/// The start of each line of the list nested under a `Check` line, as DOC-08 reads one.
const NESTED: &str = "  ";

/// The starts of the paths of the tests that a `Check` line's `ci:` items stand for: the checks'
/// own tests, and the end-to-end tests.
const CI_TESTS: [&str; 2] = ["crates/xtask/", "crates/app/tests/e2e/"];

/// A finding for each rule whose `Check` names `test` while no test is named for the rule, nor
/// for an ID that the line cites, and for each test that a rule whose `Check` doesn't name `test`
/// names, but the tests that its `ci:` items stand for. `proven` holds the tests that each ID
/// names.
pub(super) fn check(repo: &Repo, proven: &Named) -> Vec<Finding> {
    let mut findings = Vec::new();
    let rules = repo
        .files()
        .filter(|file| document_in(&file.path, "docs/standards/").is_some());
    for file in rules {
        let mut rule = None;
        // Whether the rule's `Check` line names `test`, while the lines of the list under it run.
        let mut listed = None;
        for (index, line) in file.text.lines().enumerate() {
            if let Some(heading) = line.strip_prefix("### ") {
                rule = text::id_tokens(heading)
                    .into_iter()
                    .next()
                    .filter(|token| token.column == 0)
                    .map(|token| token.id());
                listed = None;
                continue;
            }
            if !line.starts_with(NESTED) {
                listed = None;
            } else if let (Some(id), Some(false)) = (&rule, listed)
                && line.contains(TEST)
            {
                findings.push(Finding::new(
                    &file.path,
                    index + 1,
                    format!(
                        "a list under the Check of {id} names `test`, which its line doesn't: the \
                         line names each kind of check, and the IDs whose tests prove the rule, \
                         where check-test-map reads them (TEST-01)"
                    ),
                ));
            }
            let Some(id) = rule.as_ref().filter(|_| line.starts_with(CHECK)) else {
                continue;
            };
            listed = Some(line.contains(TEST));
            if !line.contains(TEST) {
                findings.extend(untested(id, proven));
            }
            let cited = text::id_tokens(line).into_iter().map(|token| token.id());
            let tested =
                proven.contains_key(id) || cited.into_iter().any(|id| proven.contains_key(&id));
            if line.contains(TEST) && !tested {
                findings.push(Finding::new(
                    &file.path,
                    index + 1,
                    format!(
                        "the Check of {id} names `test`, and no test's name starts with its ID, \
                         nor with an ID that the line cites: name a test for the rule, or cite \
                         the behaviors whose tests prove it (TEST-01)"
                    ),
                ));
            }
        }
    }
    findings
}

/// A finding for each test named by `id`, a rule whose `Check` line gives `test` nothing, but
/// those that the line's `ci:` items stand for.
fn untested(id: &str, proven: &Named) -> Vec<Finding> {
    let named = proven.get(id).map_or(&[][..], Vec::as_slice);
    named
        .iter()
        .filter(|(path, _, _)| !CI_TESTS.iter().any(|start| path.starts_with(start)))
        .map(|(path, line, name)| {
            Finding::new(
                path,
                *line,
                format!(
                    "test `{name}` is named by {id}, whose Check gives `test` nothing: the line \
                     gives `test` what the test proves, or the test is named by what it proves \
                     (TEST-01)"
                ),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::tests::run_with;

    #[test]
    fn test_01_a_rule_whose_check_names_test_needs_a_test_or_a_cited_id_with_one() {
        let rules = "# Rules\n\n\
                     ### STYLE-01 Proven by its own test\nIt holds.\n- Check: `test`.\n\n\
                     ### STYLE-02 Proven by a behavior's\nIt holds.\n- Check: `test` (WIDGETS-001).\n\n\
                     ### STYLE-03 Cites what no test proves\nIt holds.\n\
                     - Check: `review`, plus `test` (WIDGETS-002).\n\n\
                     ### STYLE-04 Cites nothing\nIt holds, as STYLE-01 does.\n- Check: `test`.\n\n\
                     ### STYLE-05 Left to review\nIt holds.\n- Check: `review` of each test.\n";
        let tests = "#[test]\nfn widgets_001_works() {}\n#[test]\nfn style_01_holds() {}\n";
        let outcome = run_with(&[
            ("docs/standards/style.md", rules),
            ("crates/widgets/src/lib.rs", tests),
        ]);

        let findings: Vec<String> = outcome
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|finding| finding.contains("names `test`"))
            .collect();
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(
            findings[0].starts_with("docs/standards/style.md:13: the Check of STYLE-03 names"),
            "{findings:?}"
        );
        assert!(
            findings[1].starts_with("docs/standards/style.md:17: the Check of STYLE-04 names"),
            "{findings:?}"
        );
    }

    #[test]
    fn test_01_a_rule_whose_check_gives_test_nothing_names_only_checks_and_end_to_end_tests() {
        let rules = "# Rules\n\n\
                     ### STYLE-01 Left to a check\nIt holds.\n- Check: `ci: check-style`.\n\n\
                     ### STYLE-02 Proven by its own test\nIt holds.\n- Check: `test`.\n";
        let named = "#[test]\nfn style_01_holds() {}\n#[test]\nfn style_02_holds() {}\n";
        let outcome = run_with(&[
            ("docs/standards/style.md", rules),
            ("crates/widgets/src/lib.rs", named),
            (
                "crates/xtask/src/style.rs",
                "#[test]\nfn style_01_is_checked() {}\n",
            ),
            (
                "crates/app/tests/e2e/style.rs",
                "#[test]\nfn style_01_holds_end_to_end() {}\n",
            ),
        ]);

        let findings: Vec<String> = outcome
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|finding| finding.contains("is named by"))
            .collect();
        assert_eq!(
            findings,
            [
                "crates/widgets/src/lib.rs:2: test `style_01_holds` is named by STYLE-01, whose \
                 Check gives `test` nothing: the line gives `test` what the test proves, or the \
                 test is named by what it proves (TEST-01)"
            ]
        );
    }

    #[test]
    fn test_01_a_list_under_a_check_line_names_test_only_where_the_line_does() {
        let rules = "# Rules\n\n\
                     ### STYLE-01 Named on the line\nIt holds.\n\
                     - Check: `test`, plus `review`, each for what its item below says.\n  \
                     - `test`: the first half.\n  - `review`: the rest.\n\n\
                     ### STYLE-02 Named in the list alone\nIt holds.\n\
                     - Check: `review`, each for what its item below says.\n  \
                     - `test` (WIDGETS-001): the first half.\n  - `review`: the rest.\n\
                     Not the list's, `test`.\n";
        let tests = "#[test]\nfn widgets_001_works() {}\n#[test]\nfn style_01_holds() {}\n";
        let outcome = run_with(&[
            ("docs/standards/style.md", rules),
            ("crates/widgets/src/lib.rs", tests),
        ]);

        let findings: Vec<String> = outcome
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|finding| finding.contains("`test`"))
            .collect();
        assert_eq!(
            findings,
            [
                "docs/standards/style.md:12: a list under the Check of STYLE-02 names `test`, \
                 which its line doesn't: the line names each kind of check, and the IDs whose \
                 tests prove the rule, where check-test-map reads them (TEST-01)"
            ]
        );
    }
}
