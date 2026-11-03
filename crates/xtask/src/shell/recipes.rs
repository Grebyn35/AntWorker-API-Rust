//! The justfile's recipes, each written as a script whose lines stand where they stand in the
//! justfile, so that a finding of shellcheck names the justfile's line.

use std::io;

use super::{INTERPOLATED, RENDERED};
use crate::repo;

/// A recipe of the justfile.
#[derive(Debug, PartialEq, Eq)]
pub struct Recipe {
    name: String,
    /// Whether it runs on Linux and macOS: it carries no attribute that limits it to Windows.
    pub unix: bool,
    /// The parameters it exports to its shell, such as `mode` for `$mode=""`.
    exported: Vec<String>,
    /// Its body, each line with its number in the justfile.
    body: Vec<(usize, String)>,
}

/// The recipes of `text`, a justfile.
pub fn recipes(text: &str) -> Vec<Recipe> {
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
pub fn render(recipe: &Recipe) -> String {
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
pub fn write_recipes(justfile: &str) -> io::Result<Vec<String>> {
    repo::empty_directory(RENDERED)?;
    let mut written = Vec::new();
    for recipe in recipes(justfile).iter().filter(|recipe| recipe.unix) {
        let path = format!("{RENDERED}/{}.sh", recipe.name);
        repo::write(&path, &render(recipe))?;
        written.push(path);
    }
    Ok(written)
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
}
