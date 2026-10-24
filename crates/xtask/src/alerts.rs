//! `alert-rules`, the part promtool doesn't run: every alert has a severity, `page` or `ticket`,
//! a runbook section that exists, and a test that expects it to fire (OBS-14). The `just` task
//! of the same name also runs promtool's checks and tests of the rules.

use std::collections::BTreeSet;

use crate::{Finding, Outcome, markdown, repo::Repo, stack};

/// The runbooks, one section per alert.
const RUNBOOKS: &str = "docs/operations/runbooks.md";

/// Runs the check.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let anchors = repo
        .file(RUNBOOKS)
        .map(|file| markdown::read(&file.text).anchors)
        .unwrap_or_default();
    let tested = fired_in_tests(repo, &mut findings);
    for rule in stack::rules(repo, &mut findings) {
        let Some(alert) = &rule.alert else {
            continue;
        };
        let mut fail = |message: String| {
            findings.push(Finding::new(
                stack::RULES,
                rule.line,
                format!("{message} (OBS-14)"),
            ));
        };
        match rule.labels["severity"].as_str() {
            Some("page" | "ticket") => {}
            _ => fail(format!("`{alert}` has no severity of page or ticket")),
        }
        let runbook = rule.annotations["runbook"].as_str().unwrap_or_default();
        match runbook.split_once('#') {
            Some((RUNBOOKS, anchor)) if anchors.contains(anchor) => {}
            _ => fail(format!(
                "`{alert}`'s runbook annotation isn't a section of {RUNBOOKS}: `{runbook}`"
            )),
        }
        if !tested.contains(alert) {
            fail(format!(
                "no test in {} expects `{alert}` to fire",
                stack::RULE_TESTS
            ))
        }
    }
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// The alerts that some test of the rules expects to fire.
fn fired_in_tests(repo: &Repo, findings: &mut Vec<Finding>) -> BTreeSet<String> {
    let Some(tests) = stack::yaml(repo, stack::RULE_TESTS, findings) else {
        return BTreeSet::new();
    };
    tests["tests"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|test| test["alert_rule_test"].as_array().into_iter().flatten())
        .filter(|case| {
            case["exp_alerts"]
                .as_array()
                .is_some_and(|alerts| !alerts.is_empty())
        })
        .filter_map(|case| case["alertname"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &str = "groups:\n  - name: a\n    rules:\n      - alert: Down\n        expr: up == 0\n        labels:\n          severity: page\n        annotations:\n          runbook: docs/operations/runbooks.md#down\n";
    const TESTS: &str = "tests:\n  - alert_rule_test:\n      - alertname: Down\n        exp_alerts:\n          - exp_labels: {severity: page}\n";

    fn findings(rules: &str, tests: &str, runbooks: &str) -> Vec<String> {
        let repo = Repo::of(&[
            (stack::RULES, rules),
            (stack::RULE_TESTS, tests),
            (RUNBOOKS, runbooks),
        ]);
        check(&repo)
            .findings
            .into_iter()
            .map(|f| f.message)
            .collect()
    }

    #[test]
    fn obs_14_an_alert_has_a_severity_a_runbook_and_a_test() {
        assert!(findings(RULES, TESTS, "# Runbooks\n\n## Down\n").is_empty());

        let unrouted = findings(&RULES.replace("page", "urgent"), TESTS, "## Down\n");
        assert_eq!(unrouted.len(), 1, "{unrouted:?}");
        let unwritten = findings(RULES, TESTS, "## Up\n");
        assert_eq!(unwritten.len(), 1, "{unwritten:?}");
        let untested = findings(RULES, "tests: []\n", "## Down\n");
        assert_eq!(untested.len(), 1, "{untested:?}");
    }
}
