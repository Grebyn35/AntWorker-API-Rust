//! `check-shell`: shellcheck finds nothing in a recipe of the justfile that runs on Linux and
//! macOS, or in a shell script under `deploy/`, and each directive that disables a finding stands
//! on the line before the command it concerns, with a comment that states its reason (CODE-16).
//!
//! `just` lists only the recipes of the platform it runs on, so the check reads the justfile
//! itself, and checks the same recipes on every platform. Each recipe becomes a script whose
//! lines stand where they stand in the justfile, so a finding's line is the justfile's. An
//! interpolation becomes a word of its own, since `just` writes its value into the line before
//! the shell reads it. shellcheck runs from the image that `compose.yaml` pins. The check reads
//! the directives itself, since shellcheck applies any directive it finds.

use std::{io, path::Path, process::Command};

use serde_json::Value;

use crate::{
    Finding, Outcome,
    repo::{self, Repo},
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
    let rendered = match write_recipes(&justfile.text) {
        Ok(rendered) => rendered,
        Err(error) => {
            findings.push(Finding::new(RENDERED, 1, error.to_string()));
            return outcome(findings);
        }
    };
    for recipe in recipes(&justfile.text).iter().filter(|recipe| recipe.unix) {
        for (line, message) in directive_problems(&render(recipe)) {
            findings.push(Finding::new(JUSTFILE, line, message));
        }
    }
    let scripts = scripts(repo);
    for path in &scripts {
        let text = repo.file(path).map_or("", |file| file.text.as_str());
        for (line, message) in directive_problems(text) {
            findings.push(Finding::new(path, line, message));
        }
    }
    let mut paths = rendered.clone();
    paths.extend(scripts);
    match shellcheck(&paths) {
        Ok(comments) => {
            for comment in comments {
                let path = if rendered.contains(&comment.path) {
                    JUSTFILE
                } else {
                    &comment.path
                };
                findings.push(Finding::new(path, comment.line, comment.message));
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

/// A recipe of the justfile.
#[derive(Debug, PartialEq, Eq)]
struct Recipe {
    name: String,
    /// Whether it runs on Linux and macOS: it carries no attribute that limits it to Windows.
    unix: bool,
    /// The parameters it exports to its shell, such as `mode` for `$mode=""`.
    exported: Vec<String>,
    /// Its body, each line with its number in the justfile.
    body: Vec<(usize, String)>,
}

/// The recipes of `text`, a justfile.
fn recipes(text: &str) -> Vec<Recipe> {
    let lines: Vec<&str> = text.lines().collect();
    let mut recipes = Vec::new();
    let mut attributes: Vec<String> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with(char::is_whitespace)
        {
            continue;
        }
        if let Some(listed) = line.strip_prefix('[') {
            let listed = listed.split(']').next().unwrap_or_default();
            attributes.extend(listed.split(',').map(|name| {
                let name = name.trim();
                name.split('(').next().unwrap_or(name).trim().to_owned()
            }));
            continue;
        }
        let Some(name) = recipe_name(line) else {
            attributes.clear();
            continue;
        };
        let mut body = Vec::new();
        while index < lines.len()
            && (lines[index].trim().is_empty() || lines[index].starts_with(char::is_whitespace))
        {
            body.push((index + 1, lines[index].to_owned()));
            index += 1;
        }
        while body.last().is_some_and(|(_, line)| line.trim().is_empty()) {
            body.pop();
        }
        let limited = attributes.iter().any(|name| name == "windows");
        let unix = attributes
            .iter()
            .any(|name| matches!(name.as_str(), "unix" | "linux" | "macos"))
            || !limited;
        recipes.push(Recipe {
            name: name.to_owned(),
            unix,
            exported: exported(line),
            body,
        });
        attributes.clear();
    }
    recipes
}

/// The name of the recipe whose header `line` is, or `None` when it is a setting, an assignment
/// or any other line that starts at the margin.
fn recipe_name(line: &str) -> Option<&str> {
    let first = line.split_whitespace().next()?;
    let keyword = ["set", "export", "unexport", "alias", "import", "mod"].contains(&first);
    let colon = line.find(':')?;
    if keyword || line[colon..].starts_with(":=") {
        return None;
    }
    let name = line[..colon]
        .split_whitespace()
        .next()?
        .trim_start_matches('@');
    let identifier = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    identifier.then_some(name)
}

/// The parameters that the recipe header `line` exports to the shell: those written `$name`.
fn exported(line: &str) -> Vec<String> {
    let parameters = line.split(':').next().unwrap_or_default();
    parameters
        .split_whitespace()
        .skip(1)
        .filter_map(|parameter| parameter.trim_start_matches(['+', '*']).strip_prefix('$'))
        .map(|parameter| parameter.split('=').next().unwrap_or(parameter).to_owned())
        .collect()
}

/// The script that `recipe` runs on Linux and macOS, with each line of its body on the line
/// where the justfile holds it. A recipe without a shebang runs each line with `sh`, `just`'s
/// shell there, after the `@` and `-` that start a line are taken off.
fn render(recipe: &Recipe) -> String {
    let shebang = recipe
        .body
        .iter()
        .find(|(_, line)| !line.trim().is_empty())
        .filter(|(_, line)| line.trim_start().starts_with("#!"));
    let mut script = vec![match shebang {
        Some((_, line)) => line.trim().to_owned(),
        None => "#!/bin/sh".to_owned(),
    }];
    // `just` sets the exported parameters in the shell's environment, so the script declares
    // them on a line the body leaves empty: the recipe's header is always above the body.
    if !recipe.exported.is_empty() {
        let values: Vec<String> = recipe
            .exported
            .iter()
            .map(|name| format!("{name}={INTERPOLATED}"))
            .collect();
        script.push(format!("export {}", values.join(" ")));
    }
    for (number, line) in &recipe.body {
        if shebang.is_some_and(|(first, _)| first == number) {
            continue;
        }
        while script.len() < number - 1 {
            script.push(String::new());
        }
        if shebang.is_some() {
            script.push(interpolate(line));
        } else {
            let content = line.trim_start();
            let indent = &line[..line.len() - content.len()];
            script.push(interpolate(&format!(
                "{indent}{}",
                content.trim_start_matches(['@', '-'])
            )));
        }
    }
    script.join("\n") + "\n"
}

/// `line` with each interpolation replaced by a word, and each `{{{{` by the `{{` it escapes.
fn interpolate(line: &str) -> String {
    let mut text = String::new();
    let mut rest = line;
    while let Some(start) = rest.find("{{") {
        text.push_str(&rest[..start]);
        rest = &rest[start..];
        if let Some(escaped) = rest.strip_prefix("{{{{") {
            text.push_str("{{");
            rest = escaped;
        } else if let Some(end) = rest.find("}}") {
            text.push_str(INTERPOLATED);
            rest = &rest[end + 2..];
        } else {
            break;
        }
    }
    text.push_str(rest);
    text
}

/// Writes the script of each recipe that runs on Linux and macOS into an empty `RENDERED`, and
/// returns the paths it wrote, from the repository root.
fn write_recipes(justfile: &str) -> io::Result<Vec<String>> {
    repo::empty_directory(RENDERED)?;
    let mut written = Vec::new();
    for recipe in recipes(justfile).iter().filter(|recipe| recipe.unix) {
        let path = format!("{RENDERED}/{}.sh", recipe.name);
        repo::write(&path, &render(recipe))?;
        written.push(path);
    }
    Ok(written)
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

/// The shell scripts under `deploy/`: the files named `*.sh`, and those whose shebang names a
/// shell.
fn scripts(repo: &Repo) -> Vec<String> {
    repo.files()
        .filter(|file| file.path.starts_with("deploy/"))
        .filter(|file| {
            let shebang = file.text.lines().next().unwrap_or_default();
            file.path.ends_with(".sh")
                || (shebang.starts_with("#!")
                    && ["sh", "bash", "dash"].iter().any(|shell| {
                        shebang.ends_with(&format!("/{shell}"))
                            || shebang.ends_with(&format!(" {shell}"))
                    }))
        })
        .map(|file| file.path.clone())
        .collect()
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

    const JUSTFILE: &str = "\
set lazy
api := \"cargo run --\"

# Formats.
fmt:
    @cargo fmt --all

[unix]
publish repository commit:
    #!/usr/bin/env bash
    set -euo pipefail

    repository={{ lowercase(repository) }}
    echo \"{{{{ .Digest }}\"

[windows]
publish repository commit:
    Write-Output '{{ repository }}'
";

    #[test]
    fn code_16_the_recipes_are_read_from_the_justfile_with_the_platforms_they_run_on() {
        let found = recipes(JUSTFILE);

        let summary: Vec<(&str, bool, usize)> = found
            .iter()
            .map(|recipe| (recipe.name.as_str(), recipe.unix, recipe.body.len()))
            .collect();
        assert_eq!(
            summary,
            [
                ("fmt", true, 1),
                ("publish", true, 5),
                ("publish", false, 1)
            ]
        );
        assert_eq!(found[1].body[0].0, 10);
    }

    #[test]
    fn code_16_a_rendered_recipe_keeps_each_line_where_the_justfile_holds_it() {
        let found = recipes(JUSTFILE);

        let fmt = render(&found[0]);
        let publish = render(&found[1]);

        let fmt: Vec<&str> = fmt.lines().collect();
        assert_eq!(fmt[0], "#!/bin/sh");
        assert_eq!(fmt.len(), 6);
        assert_eq!(fmt[5], "    cargo fmt --all");
        let publish: Vec<&str> = publish.lines().collect();
        assert_eq!(publish[0], "#!/usr/bin/env bash");
        assert_eq!(publish[10], "    set -euo pipefail");
        assert_eq!(publish[12], "    repository=just_value");
        assert_eq!(publish[13], "    echo \"{{ .Digest }}\"");
    }

    #[test]
    fn code_16_a_rendered_recipe_declares_the_parameters_it_exports() {
        let found = recipes(concat!(
            "x:\n",
            "    true\n",
            "\n",
            "dev $mode=\"\" +$rest: bootstrap\n",
            "    case \"$mode\" in *) ;; esac\n",
        ));

        let dev: Vec<String> = render(&found[1]).lines().map(str::to_owned).collect();

        assert_eq!(found[1].exported, ["mode", "rest"]);
        assert_eq!(dev[1], "export mode=just_value rest=just_value");
        assert_eq!(dev[4], "    case \"$mode\" in *) ;; esac");
    }

    #[test]
    fn code_16_an_interpolation_becomes_one_word() {
        assert_eq!(
            interpolate("{{ if a == \"b\" { \"X=1 \" } else { \"\" } }}watchexec \"{{ c }}\""),
            "just_valuewatchexec \"just_value\""
        );
    }

    #[test]
    fn code_16_settings_and_assignments_are_not_recipes() {
        assert_eq!(
            recipe_name("set windows-shell := [\"powershell.exe\"]"),
            None
        );
        assert_eq!(recipe_name("export SQLX_OFFLINE := \"true\""), None);
        assert_eq!(
            recipe_name("version := replace_regex(read(\"a\"), 'b', 'c')"),
            None
        );
        assert_eq!(
            recipe_name("image revision=`git rev-parse HEAD`: _image-builder && _image-cache"),
            Some("image")
        );
        assert_eq!(
            recipe_name("e2e $E2E_PLATFORM=native_platform: services"),
            Some("e2e")
        );
    }

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
