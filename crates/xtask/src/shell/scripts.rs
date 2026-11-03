//! The shell scripts under `deploy/`, and what check-shell reads in them itself. A script sources
//! a file of its own directory with a line `. "$here/<path>"`, where `here` names that directory.

use crate::{
    repo::Repo,
    tenancy::{referenced, unquoted},
};

/// Each line of `script` whose `echo` writes a value, an argument with an expansion. A server's
/// sh, such as Ubuntu's dash, reads a backslash in echo's arguments as an escape, and `\c` ends
/// the output, while printf's `%s` writes the value as it is. Alpine's sh, the tests' server's,
/// writes it as it is, so no test would find one.
pub fn echo_problems(script: &str) -> Vec<(usize, &'static str)> {
    let mut problems = Vec::new();
    for (index, line) in script.lines().enumerate() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let writes_value = line.match_indices("echo ").any(|(start, _)| {
            let command = start == 0
                || line[..start].ends_with(|c: char| c.is_whitespace() || "(;|&{".contains(c));
            let arguments = &line[start..];
            let end = arguments
                .find(['|', ';', '>', '&'])
                .unwrap_or(arguments.len());
            command && arguments[..end].contains('$')
        });
        if writes_value {
            problems.push((
                index + 1,
                "echo writes a value, whose backslashes a server's sh reads as escapes; write it with printf '%s\\n' (CODE-16)",
            ));
        }
    }
    problems
}

/// Each line of `script` whose `psql` takes no statement with `-c`, so a statement it reads from
/// its input would go unchecked, or whose statement names one of `tables` (ARCH-14).
pub fn psql_problems(script: &str, tables: &[String]) -> Vec<(usize, String)> {
    let mut problems = Vec::new();
    for (index, line) in script.lines().enumerate() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        for (start, _) in line.match_indices("psql") {
            let word = !line[..start].ends_with(|c: char| c.is_alphanumeric() || c == '_')
                && !line[start + 4..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
            if !word {
                continue;
            }
            let rest = &line[start..];
            let statement = rest.find(" -c ").and_then(|at| {
                let argument = rest[at + 4..].trim_start();
                let quote = argument
                    .chars()
                    .next()
                    .filter(|c| *c == '\'' || *c == '"')?;
                argument[1..].split(quote).next()
            });
            let Some(statement) = statement else {
                problems.push((
                    index + 1,
                    "psql takes its statement with -c, where check-shell reads it (ARCH-14)"
                        .to_owned(),
                ));
                continue;
            };
            for table in referenced(&unquoted(&statement.to_ascii_lowercase())) {
                if tables.contains(&table) {
                    problems.push((
                        index + 1,
                        format!(
                            "a script's statement names `{table}`, a table of the product \
                             schema: application logic runs as a subcommand of the binary \
                             (ARCH-14)"
                        ),
                    ));
                }
            }
        }
    }
    problems
}

/// The shell scripts under `deploy/`: the files named `*.sh`, and those whose shebang names a
/// shell.
pub fn scripts(repo: &Repo) -> Vec<String> {
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

/// A script, with each file it sources written in place of the line that sources it.
pub struct Inlined {
    /// Its text.
    pub text: String,
    /// The file and the line, from 1, that each of its lines comes from.
    pub origins: Vec<(String, usize)>,
    /// Whether it sources a file.
    pub sources: bool,
}

/// The file that `line` of the script at `path` sources, if it sources one.
fn sourced(path: &str, line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix(". \"$here/")?.strip_suffix('"')?;
    let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
    Some(format!("{directory}/{rest}"))
}

/// The scripts among `scripts` that no other script sources, which run on their own.
pub fn entries(repo: &Repo, scripts: &[String]) -> Vec<String> {
    let sourced: Vec<String> = scripts
        .iter()
        .filter_map(|path| repo.file(path).map(|file| (path, file)))
        .flat_map(|(path, file)| file.text.lines().filter_map(|line| sourced(path, line)))
        .collect();
    scripts
        .iter()
        .filter(|path| !sourced.contains(path))
        .cloned()
        .collect()
}

/// The script at `path`, with each file it sources in place of the line that sources it.
pub fn inline(repo: &Repo, path: &str) -> Inlined {
    let mut inlined = Inlined {
        text: String::new(),
        origins: Vec::new(),
        sources: false,
    };
    let text = repo.file(path).map_or("", |file| file.text.as_str());
    for (index, line) in text.lines().enumerate() {
        match sourced(path, line).and_then(|source| repo.file(&source)) {
            Some(file) => {
                inlined.sources = true;
                for (inner, line) in file.text.lines().enumerate() {
                    inlined.text.push_str(line);
                    inlined.text.push('\n');
                    inlined.origins.push((file.path.clone(), inner + 1));
                }
            }
            None => {
                inlined.text.push_str(line);
                inlined.text.push('\n');
                inlined.origins.push((path.to_owned(), index + 1));
            }
        }
    }
    inlined
}

/// Each line, counted from 0, where a function of `script` assigns a variable that the script's
/// top level sets: every variable of sh is global, so the assignment changes what the rest of
/// the script reads (CODE-16). A function is `name() {` at the margin, to the next `}` there.
pub fn scope_problems(script: &str) -> Vec<(usize, String)> {
    let mut top = Vec::new();
    let mut functions = Vec::new();
    let mut function: Option<String> = None;
    for (index, line) in script.lines().enumerate() {
        let code = line.trim_start();
        if code.starts_with('#') {
            continue;
        }
        if function.is_none()
            && let Some((name, rest)) = line.split_once("() {")
            && !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            let body = rest.trim_end();
            if body.ends_with('}') {
                functions.extend(
                    assigned(body)
                        .into_iter()
                        .map(|variable| (name.to_owned(), variable, index)),
                );
            } else {
                function = Some(name.to_owned());
            }
            continue;
        }
        match &function {
            Some(_) if line.starts_with('}') => function = None,
            Some(name) => functions.extend(
                assigned(line)
                    .into_iter()
                    .map(|variable| (name.clone(), variable, index)),
            ),
            None => top.extend(assigned(line)),
        }
    }
    functions
        .into_iter()
        .filter(|(_, variable, _)| top.contains(variable))
        .map(|(name, variable, index)| {
            (
                index,
                format!(
                    "the function {name} assigns {variable}, which the script's top level sets: \
                     every variable of sh is global, so a function names its own apart (CODE-16)"
                ),
            )
        })
        .collect()
}

/// The variables that `line` assigns: each word `name=...` where a command starts, before its
/// command word, and the `name` of `for name in`. Words are split outside quotes, and a command
/// starts at the line's start, after `;`, `&`, `|` and `(`, and after a keyword such as `then`.
fn assigned(line: &str) -> Vec<String> {
    const KEYWORDS: [&str; 10] = [
        "if", "then", "else", "elif", "do", "while", "until", "!", "{", "time",
    ];
    let mut names = Vec::new();
    let mut command = true;
    let mut after_for = false;
    for token in tokens(line) {
        if token.len() == 1 && ";&|()".contains(token.as_str()) {
            command = true;
            continue;
        }
        if after_for {
            names.push(token);
            after_for = false;
            command = false;
            continue;
        }
        if !command {
            continue;
        }
        if token == "for" {
            after_for = true;
        } else if let Some((name, _)) = token.split_once('=')
            && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            names.push(name.to_owned());
        } else if !KEYWORDS.contains(&token.as_str()) {
            command = false;
        }
    }
    names
}

/// The words of `line` and its separators `;`, `&`, `|`, `(` and `)`, each a token of its own,
/// split outside quotes, up to a comment.
fn tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    for c in line.chars() {
        match quote {
            Some(open) => {
                word.push(c);
                if c == open {
                    quote = None;
                }
            }
            None if c == '\'' || c == '"' => {
                word.push(c);
                quote = Some(c);
            }
            None if c == '#' && word.is_empty() => break,
            None if c.is_whitespace() || ";&|()".contains(c) => {
                if !word.is_empty() {
                    tokens.push(std::mem::take(&mut word));
                }
                if !c.is_whitespace() {
                    tokens.push(c.to_string());
                }
            }
            None => word.push(c),
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_16_a_function_names_its_variables_apart_from_the_top_level() {
        let script = "#!/bin/sh\n\
                      state=/var/lib\n\
                      for color in blue green; do :; done\n\
                      say() { printf '%s\\n' \"$*\"; }\n\
                      restore() {\n\
                      \x20   state=$1\n\
                      \x20   moment=$2 color=x\n\
                      \x20   printf '%s\\n' \"state=$state\" # state=quoted\n\
                      \x20   docker run --env state=1 image\n\
                      }\n\
                      quick() { state=2; }\n";

        let lines: Vec<usize> = scope_problems(script)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(lines, [5, 6, 10]);
    }

    #[test]
    fn code_16_a_script_is_checked_with_the_files_it_sources_in_place() {
        let repo = Repo::of(&[
            (
                "deploy/deploy.sh",
                "#!/bin/sh\nhere=$(dirname \"$0\")\n. \"$here/tasks/common.sh\"\nsay hi\n",
            ),
            (
                "deploy/tasks/common.sh",
                "# Helpers.\nsay() { printf '%s\\n' \"$*\"; }\n",
            ),
            ("deploy/verify-release.sh", "#!/bin/sh\ntrue\n"),
        ]);
        let scripts = scripts(&repo);

        let inlined = inline(&repo, "deploy/deploy.sh");

        assert_eq!(
            entries(&repo, &scripts),
            ["deploy/deploy.sh", "deploy/verify-release.sh"]
        );
        assert!(inlined.sources);
        assert_eq!(
            inlined.text.lines().nth(3),
            Some("say() { printf '%s\\n' \"$*\"; }")
        );
        assert_eq!(
            inlined.origins,
            [
                ("deploy/deploy.sh".to_owned(), 1),
                ("deploy/deploy.sh".to_owned(), 2),
                ("deploy/tasks/common.sh".to_owned(), 1),
                ("deploy/tasks/common.sh".to_owned(), 2),
                ("deploy/deploy.sh".to_owned(), 4),
            ]
        );
    }

    #[test]
    fn code_16_a_deploy_script_writes_a_value_with_printf() {
        let script = "#!/bin/sh\n\
            # echo \"$commented\"\n\
            echo \"$answer\" >&2\n\
            names=$(echo \"$listed\" | sed 's/ .*//')\n\
            echo 'deploy: nothing to do'\n\
            printf '%s\\n' \"$answer\"\n\
            left=$(cat \"$file\" || echo neither)\n";

        let problems: Vec<usize> = echo_problems(script)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [3, 4]);
    }

    #[test]
    fn arch_14_a_script_s_psql_names_no_table_of_the_product_schema() {
        let script = "#!/bin/sh\n\
                      # psql -c 'DELETE FROM users' is a comment\n\
                      ready=$(psql -qAt -c 'SELECT pg_is_in_recovery()')\n\
                      psql -qAt -c \"DELETE FROM sessions WHERE true\"\n\
                      psql -qAt < cleanup.sql\n\
                      apsql_x=1\n";
        let tables = ["sessions".to_owned(), "users".to_owned()];

        let problems: Vec<usize> = psql_problems(script, &tables)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [4, 5]);
    }
}
