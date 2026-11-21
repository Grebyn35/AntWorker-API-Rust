//! `just remove`, which removes the optional parts named; `just keep`, with which a fork chooses
//! its parts once, which removes every other, then what makes a part removable; and
//! `check-removable`, which runs `just keep` on a copy of the repository for each run that
//! `parts.yaml` lists, or `just remove` for a run that names that task, then `just checks` on
//! each (ARCH-07).

use std::process::ExitCode;

use super::{
    Part, REMOVAL, Run, delete, leftovers, list, markers, numbered, prune_cache, regenerate,
    validate,
};
use crate::{
    Finding,
    copy::{Check, Copy},
    repo::{self, Repo},
};

/// Removes the parts named `names` from the repository (ARCH-07): it deletes the files they own
/// whole and the lines their markers mark, writing "None." in each section of a spec that it
/// empties, and retiring the number of each behavior it deletes from a spec, then the cached
/// queries that no kept code holds, updates `Cargo.lock`, writes `openapi.json` and the metric
/// catalog again, formats the code, and fails while a file it kept, or the path of one, still
/// names one of them. It refuses `removal`, which only `just keep` removes.
pub fn remove(names: &[String]) -> ExitCode {
    let (repo, parts) = match loaded(names) {
        Ok(loaded) => loaded,
        Err(error) => return fail(&error),
    };
    let removed: Vec<&Part> = parts
        .iter()
        .filter(|part| names.contains(&part.name))
        .collect();
    apply(&repo, &parts, &removed, false)
}

/// Keeps the parts named `names`, and removes every other, `removal` among them, as `just remove`
/// does, then takes the markers out of every file it keeps, so that the fork's code holds none
/// (ARCH-07). It refuses a choice that keeps no part that creates workspaces, since every shape
/// has one.
pub fn keep(names: &[String]) -> ExitCode {
    let (repo, parts) = match loaded(names) {
        Ok(loaded) => loaded,
        Err(error) => return fail(&error),
    };
    match removed_by_keeping(&parts, names) {
        Ok(removed) => apply(&repo, &parts, &removed, true),
        Err(error) => fail(&error),
    }
}

/// The parts that keeping the parts `names` removes: every other part, `removal` among them; or
/// why a fork can't keep only those, since none of them creates workspaces.
fn removed_by_keeping<'a>(parts: &'a [Part], names: &[String]) -> Result<Vec<&'a Part>, String> {
    let creating: Vec<&str> = parts
        .iter()
        .filter(|part| part.creates_workspaces)
        .map(|part| part.name.as_str())
        .collect();
    if !creating
        .iter()
        .any(|part| names.iter().any(|name| name == part))
    {
        return Err(format!(
            "a product keeps one of the parts that create workspaces at least, since every shape \
             has one: {}",
            creating.join(", ")
        ));
    }
    Ok(parts
        .iter()
        .filter(|part| !names.contains(&part.name))
        .collect())
}

/// The repository and its parts, once each of `names` is a part other than `removal`.
fn loaded(names: &[String]) -> Result<(Repo, Vec<Part>), String> {
    let repo = Repo::load().map_err(|error| format!("can't read the repository: {error}"))?;
    let (parts, _) = list(&repo)?;
    refuse_unknown(&parts, names)?;
    Ok((repo, parts))
}

/// Fails unless each of `names` is one of `parts` other than `removal`, which neither task takes.
fn refuse_unknown(parts: &[Part], names: &[String]) -> Result<(), String> {
    if let Some(name) = names.iter().find(|name| *name == REMOVAL) {
        return Err(format!(
            "`{name}` is what makes the other parts removable: `just keep` removes it, with the \
             parts that the fork doesn't keep"
        ));
    }
    let known: Vec<&str> = parts
        .iter()
        .map(|part| part.name.as_str())
        .filter(|name| *name != REMOVAL)
        .collect();
    match names.iter().find(|name| !known.contains(&name.as_str())) {
        Some(unknown) => Err(format!(
            "no part is named `{unknown}`; the parts are: {}",
            known.join(", ")
        )),
        None => Ok(()),
    }
}

/// Removes the parts `removed` from `repo`, as [`remove`] says, and takes the markers out of the
/// files it keeps when `unmark` holds, as [`keep`] does.
fn apply(repo: &Repo, parts: &[Part], removed: &[&Part], unmark: bool) -> ExitCode {
    let known: Vec<&str> = parts.iter().map(|part| part.name.as_str()).collect();
    let findings = validate(repo, parts);
    if !findings.is_empty() {
        return failures(
            "the parts' markers or files are wrong, so nothing was removed",
            &findings,
        );
    }
    let removing: Vec<&str> = removed.iter().map(|part| part.name.as_str()).collect();
    let (deleted, edited) = match delete(repo, removed, &known) {
        Ok(counts) => counts,
        Err(error) => return fail(&error),
    };
    let unmarked = if unmark {
        match unmark_kept() {
            Ok(unmarked) => format!(", and took the markers out of {unmarked}"),
            Err(error) => return fail(&error),
        }
    } else {
        String::new()
    };
    let stale = match prune_cache() {
        Ok(stale) => stale,
        Err(error) => return fail(&error),
    };
    report(&format!(
        "removed {}: deleted {deleted} files and {stale} entries of the query cache, and edited \
         {edited}{unmarked}",
        removing.join(", "),
    ));
    if let Err(error) = regenerate() {
        return fail(&error);
    }
    match Repo::load() {
        Ok(repo) => {
            let files = repo
                .files()
                .map(|file| (file.path.as_str(), numbered(&file.text)));
            let findings = leftovers(repo.paths(), files, removed);
            if findings.is_empty() {
                ExitCode::SUCCESS
            } else {
                failures(
                    "these lines still name what was removed: give each its part's marker, or \
                     rewrite it not to name the part (ARCH-07)",
                    &findings,
                )
            }
        }
        Err(error) => fail(&format!("can't read the repository: {error}")),
    }
}

/// Takes the markers out of every file of the repository, as [`markers::unmark`] says, and
/// returns how many files it changed.
fn unmark_kept() -> Result<usize, String> {
    let repo = Repo::load().map_err(|error| format!("can't read the repository: {error}"))?;
    let mut changed = 0;
    for file in repo.files() {
        if let Some(text) = markers::unmark(&file.text) {
            repo::write(&file.path, &text)
                .map_err(|error| format!("can't write {}: {error}", file.path))?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// Runs `check-removable` (ARCH-07): for each of the runs named `names`, or every run, on a copy
/// of the repository of its own, `just keep` with every part that the run doesn't remove, other
/// than `removal`, or, for a run whose task is `remove`, `just remove` with the run's parts, then
/// `just checks`.
pub fn check_removable(names: &[String]) -> ExitCode {
    let repo = match Repo::load() {
        Ok(repo) => repo,
        Err(error) => return fail(&format!("can't read the repository: {error}")),
    };
    let (parts, runs) = match list(&repo) {
        Ok(list) => list,
        Err(error) => return fail(&error),
    };
    let findings = validate(&repo, &parts);
    if !findings.is_empty() {
        return failures("the parts' markers or files are wrong", &findings);
    }
    if let Some(unknown) = names
        .iter()
        .find(|name| !runs.iter().any(|run| &run.name == *name))
    {
        return fail(&format!("no run is named `{unknown}`"));
    }
    let mut failed = Vec::new();
    for run in runs
        .iter()
        .filter(|run| names.is_empty() || names.contains(&run.name))
    {
        if run.parts.is_empty() {
            report(&format!(
                "{}: every part it removes is removed already",
                run.name
            ));
            continue;
        }
        let task = task(&parts, run);
        report(&format!("{}: just {}", run.name, task.join(" ")));
        match changed(&run.name, &task) {
            Ok(()) => report(&format!("{}: passed", run.name)),
            Err(error) => {
                report(&format!("{}: failed: {error}", run.name));
                failed.push(run.name.clone());
            }
        }
    }
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        fail(&format!("these runs failed: {}", failed.join(", ")))
    }
}

/// The task that `run` gives `just` on its copy, with its arguments: `keep` and every part that
/// the run doesn't remove, other than `removal`, as a fork that chooses its parts runs it, or,
/// for a run whose task is `remove`, `remove` and the run's parts, as a fork that has yet to
/// choose runs it.
fn task<'a>(parts: &'a [Part], run: &'a Run) -> Vec<&'a str> {
    if run.keeps_removal {
        return ["remove"]
            .into_iter()
            .chain(run.parts.iter().map(String::as_str))
            .collect();
    }
    ["keep"]
        .into_iter()
        .chain(
            parts
                .iter()
                .map(|part| part.name.as_str())
                .filter(|part| *part != REMOVAL && !run.parts.iter().any(|name| name == part)),
        )
        .collect()
}

/// On a fresh copy of the repository for the run `name`, runs `just` with `task`, then `just
/// checks`, as on a fork that has run the task (ARCH-07).
fn changed(name: &str, task: &[&str]) -> Result<(), String> {
    let copy = Copy::fresh(Check::Removable, name)?;
    copy.just(task)?;
    copy.just(&["checks"])
}

#[expect(clippy::print_stdout, reason = "the report is the task's output")]
fn report(message: &str) {
    println!("{message}");
}

#[expect(clippy::print_stderr, reason = "the error is the task's output")]
fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[expect(clippy::print_stderr, reason = "the findings are the task's output")]
fn failures(message: &str, findings: &[Finding]) -> ExitCode {
    for finding in findings {
        eprintln!("{finding}");
    }
    fail(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(name: &str, creates_workspaces: bool) -> Part {
        Part {
            name: name.to_owned(),
            module: None,
            paths: Vec::new(),
            words: Vec::new(),
            creates_workspaces,
        }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn arch_07_keeping_parts_removes_every_other_and_removal() {
        let parts = [
            part(REMOVAL, false),
            part("teams", true),
            part("files", false),
        ];
        let removed = removed_by_keeping(&parts, &names(&["teams"])).unwrap();
        let removed: Vec<&str> = removed.iter().map(|part| part.name.as_str()).collect();
        assert_eq!(removed, [REMOVAL, "files"]);
    }

    #[test]
    fn arch_07_a_fork_keeps_a_part_that_creates_workspaces() {
        let parts = [
            part(REMOVAL, false),
            part("teams", true),
            part("files", false),
        ];
        let error = removed_by_keeping(&parts, &names(&["files"]))
            .err()
            .unwrap();
        assert!(
            error.ends_with("since every shape has one: teams"),
            "{error}"
        );
    }

    #[test]
    fn arch_07_a_run_keeps_every_other_part_or_removes_its_own_with_the_task_it_names() {
        let parts = [
            part(REMOVAL, false),
            part("teams", true),
            part("files", false),
            part("offers", false),
        ];
        let run = |keeps_removal| Run {
            name: "shop".to_owned(),
            parts: names(&["files", "offers"]),
            keeps_removal,
        };

        assert_eq!(task(&parts, &run(false)), ["keep", "teams"]);
        assert_eq!(task(&parts, &run(true)), ["remove", "files", "offers"]);
    }

    #[test]
    fn arch_07_neither_task_takes_removal_or_a_part_that_doesn_t_exist() {
        let parts = [part(REMOVAL, false), part("teams", true)];
        assert!(refuse_unknown(&parts, &names(&["teams"])).is_ok());
        for named in [REMOVAL, "coupons"] {
            assert!(
                refuse_unknown(&parts, &names(&["teams", named])).is_err(),
                "{named}"
            );
        }
    }
}
