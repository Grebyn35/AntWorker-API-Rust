//! CI's `removable` job against the runs of `parts.yaml`: the job makes one run for each name
//! of its `matrix.run`, which a developer writes by hand, so the check holds that list to the
//! runs' names, each once, on the repository and on what each removal that keeps the runs
//! leaves of the two files (ARCH-07). A repository without the workflow has no such job to
//! hold.

use serde_json::Value;

/// The workflow that holds the job, from the repository root.
pub(super) const WORKFLOW: &str = ".github/workflows/ci.yml";

/// What keeps the job's runs from being the runs of the list: each name of `list`'s `runs`
/// that the `matrix.run` of `workflow`'s job `removable` lacks, each name of that matrix that
/// no run has, and each that it holds twice; or that one of the two can't be read as YAML that
/// holds those names as strings. `list` and `workflow` are the files' texts.
pub(super) fn disagreements(list: &str, workflow: &str) -> Vec<String> {
    let names = |text: &str, at: &[&str], name: Option<&str>| -> Option<Vec<String>> {
        let value: Value = serde_saphyr::from_str(text).ok()?;
        let entries = at.iter().try_fold(&value, |value, key| value.get(key))?;
        let entries = entries.as_array()?.iter();
        entries
            .map(|entry| name.map_or(Some(entry), |name| entry.get(name)))
            .map(|entry| entry.and_then(Value::as_str).map(str::to_owned))
            .collect()
    };
    let runs = names(list, &["runs"], Some("name"));
    let matrix = names(
        workflow,
        &["jobs", "removable", "strategy", "matrix", "run"],
        None,
    );
    let (Some(runs), Some(matrix)) = (runs, matrix) else {
        return vec![
            "check-parts can't read the names of the runs, or those of the `matrix.run` of \
             the job `removable`, as lists of strings (ARCH-07)"
                .to_owned(),
        ];
    };
    let mut found = Vec::new();
    for run in runs.iter().filter(|run| !matrix.contains(run)) {
        found.push(format!(
            "the job `removable` makes no run `{run}`, which the list of the runs holds: add \
             it to the job's `matrix.run` (ARCH-07)"
        ));
    }
    for (index, name) in matrix.iter().enumerate() {
        if !runs.contains(name) {
            found.push(format!(
                "the job `removable` makes a run `{name}`, which the list of the runs doesn't \
                 hold (ARCH-07)"
            ));
        } else if matrix[..index].contains(name) {
            found.push(format!(
                "the job `removable` makes the run `{name}` twice (ARCH-07)"
            ));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::disagreements;

    const LIST: &str = "parts:\nruns:\n  - name: every_part\n    parts:\n  \
                        # part: offers\n  - name: offers\n    parts:\n      - offers\n  \
                        # end part: offers\n";

    fn workflow(runs: &str) -> String {
        format!(
            "jobs:\n  checks:\n    steps:\n      - run: just ci\n  removable:\n    \
             strategy:\n      matrix:\n        run:\n{runs}    steps:\n      \
             - run: just check-removable ${{{{ matrix.run }}}}\n"
        )
    }

    #[test]
    fn arch_07_the_removable_job_makes_each_run_of_the_list_once() {
        let agreeing = workflow(
            "          - every_part\n          # part: offers\n          - offers\n          \
             # end part: offers\n",
        );
        assert_eq!(disagreements(LIST, &agreeing), Vec::<String>::new());

        let differing =
            workflow("          - every_part\n          - every_part\n          - coupons\n");
        assert_eq!(
            disagreements(LIST, &differing),
            [
                "the job `removable` makes no run `offers`, which the list of the runs holds: \
                 add it to the job's `matrix.run` (ARCH-07)",
                "the job `removable` makes the run `every_part` twice (ARCH-07)",
                "the job `removable` makes a run `coupons`, which the list of the runs doesn't \
                 hold (ARCH-07)",
            ]
        );
    }

    #[test]
    fn arch_07_a_job_or_a_list_that_holds_no_such_names_is_found() {
        let unread = "check-parts can't read the names of the runs, or those of the \
                      `matrix.run` of the job `removable`, as lists of strings (ARCH-07)";
        for (list, workflow) in [
            (LIST, "jobs:\n  checks:\n    steps:\n".to_owned()),
            (LIST, workflow("          - 1\n")),
            (LIST, "jobs: [".to_owned()),
            ("parts:\n", workflow("          - every_part\n")),
            ("runs:\n  - parts:\n", workflow("          - every_part\n")),
        ] {
            assert_eq!(
                disagreements(list, &workflow),
                [unread],
                "{list} {workflow}"
            );
        }
    }
}
