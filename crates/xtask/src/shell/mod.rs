//! `check-shell`: shellcheck finds nothing in a recipe of the justfile that runs on Linux and
//! macOS, or in a shell script under `deploy/`, and each directive that disables a finding stands
//! on the line before the command it concerns, with a comment that states its reason. A script
//! under `deploy/` writes a value with printf, never echo, and none of its functions assigns a
//! variable that its top level sets (CODE-16). Each `psql` of a script under `deploy/` takes its
//! statement with `-c`, and the statement names no table of the product schema (ARCH-14).
//!
//! shellcheck reports nothing in a file that a script sources, and finds the script's variables
//! unassigned when it checks that file alone, so each script is checked with the files it
//! sources written in place of the lines that source them (`scripts`), and a finding names the
//! file and line it comes from.
//!
//! `just` lists only the recipes of the platform it runs on, so the check reads the justfile
//! itself, and checks the same recipes on every platform. Each recipe becomes a script whose
//! lines stand where they stand in the justfile, so a finding's line is the justfile's. An
//! interpolation becomes a word of its own, since `just` writes its value into the line before
//! the shell reads it. shellcheck runs from the image that `compose.yaml` pins. The check reads
//! the directives itself, since shellcheck applies any directive it finds.

mod recipes;
mod scripts;

use std::{collections::BTreeMap, path::Path, process::Command};

use serde_json::Value;

pub use scripts::scripts;

use crate::{
    Finding, Outcome,
    repo::{self, Repo},
    tenancy,
};

const JUSTFILE: &str = "justfile";
/// Where the scripts made from the justfile's recipes go, from the repository root.
const RENDERED: &str = "target/check-shell";
/// The word an interpolation becomes.
const INTERPOLATED: &str = "just_value";

/// Runs the check.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let Some(justfile) = repo.file(JUSTFILE) else {
        findings.push(Finding::new(JUSTFILE, 1, "the justfile is missing"));
        return outcome(findings);
    };
    let rendered = match recipes::write_recipes(&justfile.text) {
        Ok(rendered) => rendered,
        Err(error) => {
            findings.push(Finding::new(RENDERED, 1, error.to_string()));
            return outcome(findings);
        }
    };
    for recipe in recipes::recipes(&justfile.text)
        .iter()
        .filter(|recipe| recipe.unix)
    {
        for (line, message) in directive_problems(&recipes::render(recipe)) {
            findings.push(Finding::new(JUSTFILE, line, message));
        }
    }
    let scripts = scripts::scripts(repo);
    let tables: Vec<String> = tenancy::tables(repo).into_keys().collect();
    for path in &scripts {
        let text = repo.file(path).map_or("", |file| file.text.as_str());
        for (line, message) in directive_problems(text)
            .into_iter()
            .chain(scripts::echo_problems(text))
            .map(|(line, message)| (line, message.to_owned()))
            .chain(scripts::psql_problems(text, &tables))
        {
            findings.push(Finding::new(path, line, message));
        }
    }
    // Each script that no other sources, with the files it sources in place: its path for
    // shellcheck, and where each of its lines comes from.
    let mut paths = rendered.clone();
    let mut origins: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
    let inlined_directory = format!("{RENDERED}/scripts");
    if let Err(error) = repo::empty_directory(&inlined_directory) {
        findings.push(Finding::new(RENDERED, 1, error.to_string()));
        return outcome(findings);
    }
    for path in scripts::entries(repo, &scripts) {
        let inlined = scripts::inline(repo, &path);
        for (index, message) in scripts::scope_problems(&inlined.text) {
            let (file, line) = &inlined.origins[index];
            findings.push(Finding::new(file, *line, message));
        }
        if inlined.sources {
            let written = format!("{inlined_directory}/{}", path.replace('/', "-"));
            if let Err(error) = repo::write(&written, &inlined.text) {
                findings.push(Finding::new(&path, 1, error.to_string()));
                continue;
            }
            paths.push(written.clone());
            origins.insert(written, inlined.origins);
        } else {
            paths.push(path);
        }
    }
    match shellcheck(&paths) {
        Ok(comments) => {
            for comment in comments {
                let (path, line) = if rendered.contains(&comment.path) {
                    (JUSTFILE.to_owned(), comment.line)
                } else if let Some(origin) = origins
                    .get(&comment.path)
                    .and_then(|origins| origins.get(comment.line.saturating_sub(1)))
                {
                    origin.clone()
                } else {
                    (comment.path.clone(), comment.line)
                };
                findings.push(Finding::new(&path, line, comment.message));
            }
        }
        Err(error) => findings.push(Finding::new(JUSTFILE, 1, error)),
    }
    outcome(findings)
}

fn outcome(findings: Vec<Finding>) -> Outcome {
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// The directives of `script` that disable a finding where CODE-16 doesn't allow it, each with its
/// line, counted from 1: one before the script's first command, which shellcheck applies to the
/// whole script; one with no command on the line after it; and one with no comment after it that
/// states its reason.
fn directive_problems(script: &str) -> Vec<(usize, &'static str)> {
    let mut problems = Vec::new();
    let mut commanded = false;
    let lines: Vec<&str> = script.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let content = line.trim_start();
        let Some(directive) = content.strip_prefix("# shellcheck disable=") else {
            if !content.is_empty() && !content.starts_with('#') {
                commanded = true;
            }
            continue;
        };
        if !commanded {
            problems.push((
                index + 1,
                "the directive comes before the script's first command, so it disables the finding in the whole script (CODE-16)",
            ));
        }
        let next = lines.get(index + 1).map_or("", |line| line.trim_start());
        if next.is_empty() || next.starts_with('#') {
            problems.push((
                index + 1,
                "the directive stands on the line before the command it concerns (CODE-16)",
            ));
        }
        if !directive
            .split_once(" # ")
            .is_some_and(|(_, reason)| !reason.trim().is_empty())
        {
            problems.push((
                index + 1,
                "the directive states its reason in a comment after it, as `# shellcheck disable=SC2086 # why` (CODE-16)",
            ));
        }
    }
    problems
}

/// One finding of shellcheck.
#[derive(Debug, PartialEq, Eq)]
struct Comment {
    path: String,
    line: usize,
    message: String,
}

/// Runs shellcheck on `paths`, from the repository root, and returns what it found.
fn shellcheck(paths: &[String]) -> Result<Vec<Comment>, String> {
    let output = Command::new("docker")
        .args([
            "compose",
            "run",
            "--rm",
            "--no-deps",
            "--no-TTY",
            "--volume",
            ".:/repository:ro",
            "--workdir",
            "/repository",
            "shellcheck",
            "--format=json1",
        ])
        .args(paths)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .map_err(|error| format!("can't run shellcheck through Docker Compose: {error}"))?;
    // shellcheck exits 1 when it finds something, and with other codes when it can't check.
    if !matches!(output.status.code(), Some(0 | 1)) {
        return Err(format!(
            "shellcheck didn't run: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    comments(&String::from_utf8_lossy(&output.stdout))
}

/// The findings in `json`, shellcheck's output in its `json1` format.
fn comments(json: &str) -> Result<Vec<Comment>, String> {
    let value: Value = serde_json::from_str(json)
        .map_err(|error| format!("shellcheck's output isn't JSON: {error}"))?;
    let listed = value["comments"]
        .as_array()
        .ok_or("shellcheck's output lists no comments")?;
    Ok(listed
        .iter()
        .map(|comment| Comment {
            path: comment["file"].as_str().unwrap_or_default().to_owned(),
            line: comment["line"]
                .as_u64()
                .and_then(|line| usize::try_from(line).ok())
                .unwrap_or(1),
            message: format!(
                "SC{} ({}): {} (CODE-16)",
                comment["code"],
                comment["level"].as_str().unwrap_or_default(),
                comment["message"].as_str().unwrap_or_default()
            ),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_16_a_directive_before_a_command_of_its_own_with_its_reason_passes() {
        let script = "#!/bin/sh\nset -eu\nf() {\n    # shellcheck disable=SC2086 # $a holds one word per option.\n    ls $a\n}\n";

        assert_eq!(directive_problems(script), []);
    }

    #[test]
    fn code_16_a_directive_for_the_whole_script_apart_from_its_command_or_without_a_reason_fails() {
        let whole = "#!/bin/sh\n# shellcheck disable=SC2086 # every word splits.\nls $a\n";
        let apart = "#!/bin/sh\nset -eu\n# shellcheck disable=SC2086 # $a splits.\n\nls $a\n";
        let unexplained = "#!/bin/sh\nset -eu\n# shellcheck disable=SC2086\nls $a\n";

        assert_eq!(directive_problems(whole).len(), 1);
        assert!(directive_problems(whole)[0].1.contains("whole script"));
        assert_eq!(directive_problems(apart).len(), 1);
        assert!(
            directive_problems(apart)[0]
                .1
                .contains("line before the command")
        );
        assert_eq!(directive_problems(unexplained).len(), 1);
        assert!(directive_problems(unexplained)[0].1.contains("its reason"));
    }

    #[test]
    fn code_16_shellcheck_s_comments_become_findings() {
        let json = r#"{"comments":[{"file":"deploy/deploy.sh","line":7,"endLine":7,"column":3,"endColumn":9,"level":"warning","code":2086,"message":"Double quote to prevent globbing and word splitting.","fix":null}]}"#;

        assert_eq!(
            comments(json).unwrap(),
            [Comment {
                path: "deploy/deploy.sh".to_owned(),
                line: 7,
                message: "SC2086 (warning): Double quote to prevent globbing and word splitting. (CODE-16)".to_owned(),
            }]
        );
    }
}
