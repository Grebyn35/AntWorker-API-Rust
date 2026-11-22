//! TEST-01, for the routes that a behavior's tests send to. `just test` names a file in
//! `SENT_ROUTES`, in which the API of each integration test records each request that one of
//! its routes takes: a line of the test's name, the request's method and the route's path,
//! with a tab between them (the tests' `support/sent.rs`). Once the tests pass, the task runs
//! this check on the file.
//!
//! The check reads each row of the Routes table of each spec: the routes of its first cell, each
//! a method and a path between backticks, and the behaviors of its last, each an ID, or the IDs
//! from one to another of its module, written `<ID> to <ID>`, outside an HTML comment. A
//! line's test is named for a behavior when its name, after its last `::`, starts with the
//! behavior's ID. For each behavior of a row that some line's test is named for, the check fails
//! on each route of the row that no such line holds, the route's method and its path as the row
//! writes them. It passes over a behavior that no line's test is named for, such as one whose
//! tests read the document, or send to a deployed server.
//!
//! The test framework names a test's thread after the test only when it runs the tests on more
//! than one thread. A file whose every line names the thread `main` came from a run on one, and
//! the check reads nothing of it, and says so.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use super::id_of;
use crate::{Finding, Outcome, catalog::document_in, markdown, repo::Repo};

/// The name of the thread that runs every test of a run on one thread.
const ONE_THREAD: &str = "main";

/// Checks the specs' Routes tables against the file at `path`, which the tests wrote: a file
/// that can't be read is a finding.
pub fn check_file(repo: &Repo, path: &Path) -> Outcome {
    match crate::repo::read(path) {
        Ok(sent) => check(repo, &String::from_utf8_lossy(&sent)),
        Err(error) => Outcome {
            report: Vec::new(),
            findings: vec![Finding::new(
                &path.to_string_lossy(),
                1,
                format!("can't read the routes that the tests sent to: {error}"),
            )],
        },
    }
}

/// Checks the specs' Routes tables against `sent`, the text of the file that the tests wrote.
fn check(repo: &Repo, sent: &str) -> Outcome {
    let mut outcome = Outcome::default();
    let lines: Vec<(&str, &str)> = sent
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    if !lines.is_empty() && lines.iter().all(|(test, _)| *test == ONE_THREAD) {
        outcome.report.push(
            "sent-routes: the tests ran on one thread, which names no test, so the routes \
             that each behavior's tests sent to aren't read (TEST-01)"
                .to_owned(),
        );
        return outcome;
    }
    // The routes that the tests named for each behavior sent to, each as `<method> <path>`.
    let mut sent_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (test, request) in lines {
        let name = test.rsplit("::").next().unwrap_or(test);
        if let Some(id) = id_of(name) {
            sent_by
                .entry(id)
                .or_default()
                .insert(request.replace('\t', " "));
        }
    }
    let (mut read, mut passed_over) = (0, BTreeSet::new());
    let specs = repo
        .files()
        .filter(|file| document_in(&file.path, "docs/spec/").is_some());
    for file in specs {
        let tables = markdown::tables(&file.text);
        let rows = tables
            .iter()
            .filter(|table| table.section.as_deref() == Some("Routes"))
            .flat_map(|table| &table.rows);
        for (line, cells) in rows {
            let (Some(first), Some(last)) = (cells.first(), cells.last()) else {
                continue;
            };
            let routes = routes(first);
            for behavior in behaviors(last) {
                let Some(sent) = sent_by.get(&behavior) else {
                    passed_over.insert(behavior);
                    continue;
                };
                for route in &routes {
                    read += 1;
                    if !sent.contains(*route) {
                        outcome.findings.push(Finding::new(
                            &file.path,
                            *line,
                            format!(
                                "no test named for {behavior} sent `{route}`, for which this \
                                 row lists the behavior: have a test of the behavior send it, \
                                 or take the behavior out of the row (TEST-01)"
                            ),
                        ));
                    }
                }
            }
        }
    }
    outcome.report.push(format!(
        "sent-routes: read {read} pairs of a behavior and a route that a spec's Routes table \
         lists it for, and passed over {} behaviors that no test that sent a request is named \
         for.",
        passed_over.len()
    ));
    outcome
}

/// The routes of a Routes row's first cell: each text between backticks that holds a method in
/// capitals, a space and a path that starts with a slash.
fn routes(cell: &str) -> Vec<&str> {
    cell.split('`')
        .skip(1)
        .step_by(2)
        .filter(|route| {
            route.split_once(' ').is_some_and(|(method, path)| {
                !method.is_empty()
                    && method.bytes().all(|byte| byte.is_ascii_uppercase())
                    && path.starts_with('/')
            })
        })
        .collect()
}

/// The behaviors of a Routes row's last cell, outside an HTML comment: each ID between its
/// commas, and each ID from one to another of its module, written `<ID> to <ID>`.
fn behaviors(cell: &str) -> Vec<String> {
    let listed = cell.split("<!--").next().unwrap_or(cell);
    let mut ids = Vec::new();
    for part in listed.split(',').map(str::trim) {
        match part.split_once(" to ") {
            None => ids.push(part.to_owned()),
            Some((first, last)) => {
                let numbers = |id: &str| {
                    let (prefix, number) = id.rsplit_once('-')?;
                    Some((prefix.to_owned(), number.parse::<u32>().ok()?))
                };
                if let (Some((prefix, first)), Some((of_last, last))) =
                    (numbers(first), numbers(last))
                    && prefix == of_last
                {
                    ids.extend((first..=last).map(|number| format!("{prefix}-{number:03}")));
                }
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::fixture_with;

    const SPEC: &str = "# Spec\n\n## Routes\n\n| Method and path | Access | Behaviors |\n|---|---|---|\n\
                        | `GET /v1/widgets` | user | WIDGETS-001, WIDGETS-002 |\n\
                        | `POST /v1/widgets`, `PUT /v1/widgets/{widget_id}` | user | WIDGETS-001 to WIDGETS-003 <!-- WIDGETS-009 --> |\n\n\
                        ## Behaviors\n\n| ID | Behavior | Rules | Test prefix |\n|---|---|---|---|\n\
                        | WIDGETS-001 | One | STYLE-01 | `widgets_001_` |\n";

    /// What the check finds in the spec with the lines `sent`.
    fn found(sent: &str) -> Outcome {
        let repo = Repo::of(&fixture_with(&[("docs/spec/widgets.md", SPEC)]));
        check(&repo, sent)
    }

    #[test]
    fn test_01_a_route_that_no_test_of_a_row_s_behavior_sent_fails() {
        // Tests of the first behavior sent two of its three routes, one of them with another
        // method too; a test of the second sent another row's route alone; and no test of the
        // third sent anything.
        let sent = "widgets::widgets_001_lists\tGET\t/v1/widgets\n\
                    widgets::more::widgets_001_changes\tPUT\t/v1/widgets/{widget_id}\n\
                    widgets::more::widgets_001_changes\tGET\t/v1/widgets/{widget_id}\n\
                    widgets_002_makes\tPOST\t/v1/widgets\n\
                    gadgets::gadgets_001_makes\tPOST\t/v1/widgets\n";

        let outcome = found(sent);

        let findings: Vec<String> = outcome.findings.iter().map(ToString::to_string).collect();
        assert_eq!(findings.len(), 3, "{findings:#?}");
        assert!(
            findings[0].starts_with(
                "docs/spec/widgets.md:7: no test named for WIDGETS-002 sent `GET /v1/widgets`"
            ),
            "{findings:#?}"
        );
        assert!(
            findings[1].starts_with(
                "docs/spec/widgets.md:8: no test named for WIDGETS-001 sent `POST /v1/widgets`"
            ),
            "{findings:#?}"
        );
        assert!(
            findings[2].starts_with(
                "docs/spec/widgets.md:8: no test named for WIDGETS-002 sent \
                 `PUT /v1/widgets/{widget_id}`"
            ),
            "{findings:#?}"
        );
        assert_eq!(
            outcome.report,
            [
                "sent-routes: read 6 pairs of a behavior and a route that a spec's Routes table \
              lists it for, and passed over 1 behaviors that no test that sent a request is \
              named for."
            ]
        );
    }

    #[test]
    fn test_01_a_run_on_one_thread_names_no_test_and_is_not_read() {
        let outcome = found("main\tGET\t/v1/widgets\nmain\tPOST\t/v1/widgets\n");

        assert!(outcome.findings.is_empty());
        assert_eq!(outcome.report.len(), 1);
        assert!(outcome.report[0].contains("ran on one thread"));
    }

    #[test]
    fn test_01_a_row_s_behaviors_are_its_ids_and_ranges_outside_a_comment() {
        assert_eq!(
            behaviors("A_B-009 to A_B-011, C-001 <!-- a note, D-001 -->"),
            ["A_B-009", "A_B-010", "A_B-011", "C-001"]
        );
        assert_eq!(
            routes("`GET /docs`, `GET /docs/openapi.json` and `not a route`"),
            ["GET /docs", "GET /docs/openapi.json"]
        );
    }
}
