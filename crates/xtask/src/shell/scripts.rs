//! The shell scripts under `deploy/`, and what check-shell reads in them itself: each `echo` that
//! writes a value ([`echo_problems`]), each function that assigns a variable of the top level
//! ([`scope_problems`]), and each `psql`'s statement ([`psql_problems`]). A script sources a file
//! of its own directory with a line `. "$here/<path>"`, where `here` names that directory.

use std::collections::BTreeSet;

use crate::{repo::Repo, tenancy::words};

/// Each line of `script` whose `echo`, the word `echo ` where a command starts, writes a value: an
/// argument with an expansion, as [`writes_expansion`] reads it on the `echo`'s line. A server's
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
            command && writes_expansion(&line[start + 5..])
        });
        if writes_value {
            problems.push((
                index + 1,
                "echo writes a value, whose backslashes a server's sh reads as escapes; write it with printf '%s\\n' (CODE-07)",
            ));
        }
    }
    problems
}

/// Whether `arguments`, the text after an `echo `, holds a `$` or a backtick outside single
/// quotes, which no backslash escapes, before the command ends at a `;`, `|`, `&`, `)` or a `#`
/// that starts a word, outside quotes. The `&` of a redirection, as in `>&2`, ends nothing, and
/// the word after a `>` or a `<` is the redirection's, no argument.
fn writes_expansion(arguments: &str) -> bool {
    let mut quote = None;
    // Whether the next word, or the one being read, is a redirection's.
    let mut redirected = false;
    let mut previous = ' ';
    let mut chars = arguments.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') if c == '\'' => quote = None,
            Some('\'') => {}
            _ if c == '\\' => {
                chars.next();
            }
            Some(_) if c == '"' => quote = None,
            None if c == '\'' || c == '"' => quote = Some(c),
            _ if c == '$' || c == '`' => {
                if !redirected {
                    return true;
                }
            }
            Some(_) => {}
            None if c == '>' || c == '<' => redirected = true,
            None if c == '&' && (previous == '>' || previous == '<') => {}
            None if ";|&)".contains(c) || (c == '#' && previous.is_whitespace()) => return false,
            None if c.is_whitespace()
                && !(previous.is_whitespace() || "<>&".contains(previous)) =>
            {
                redirected = false;
            }
            None => {}
        }
        previous = c;
    }
    false
}

/// Each line of `script` whose `psql` takes no statement with `-c`, so a statement it reads from
/// its input would go unchecked, whose statement, the text between the quotes after its ` -c `,
/// holds a `$` or a backtick inside double quotes, whose value the check can't read, or whose
/// statement holds a word, outside its strings and in any case, that is one of `tables` (ARCH-14).
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
                Some((quote, argument[1..].split(quote).next()?))
            });
            let Some((quote, statement)) = statement else {
                problems.push((
                    index + 1,
                    "psql takes its statement with -c, where check-shell reads it (ARCH-14)"
                        .to_owned(),
                ));
                continue;
            };
            if quote == '"' && statement.contains(['$', '`']) {
                problems.push((
                    index + 1,
                    "psql's statement holds an expansion, whose value check-shell can't read: \
                     write the statement itself after -c (ARCH-14)"
                        .to_owned(),
                ));
                continue;
            }
            let named: BTreeSet<String> =
                words(statement).into_iter().map(|(word, _)| word).collect();
            for table in named {
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
/// the script reads (CODE-07). A function starts on a line that [`defined`] reads, and runs to
/// the next line that starts with `}` at that line's indentation, or ends on its first line when
/// that line ends with `}`. A function whose body is in parentheses runs in a subshell, whose
/// assignments the script never sees, so the check reads its lines as the top level's.
pub fn scope_problems(script: &str) -> Vec<(usize, String)> {
    let mut top = Vec::new();
    let mut functions = Vec::new();
    // The function being read, and the indentation of its first line.
    let mut function: Option<(String, usize)> = None;
    // A function whose `{` the next line holds.
    let mut braceless: Option<(String, usize)> = None;
    for (index, line) in script.lines().enumerate() {
        let code = line.trim_start();
        let indentation = line.len() - code.len();
        if code.starts_with('#') {
            continue;
        }
        let mut body = None;
        if let Some((name, at)) = braceless.take()
            && let Some(rest) = code.strip_prefix('{')
        {
            function = Some((name, at));
            body = Some(rest);
        } else if function.is_none()
            && let Some((name, rest)) = defined(code)
        {
            match rest {
                None => braceless = Some((name.to_owned(), indentation)),
                Some(rest) if rest.trim_end().ends_with('}') => functions.extend(
                    assigned(rest)
                        .into_iter()
                        .map(|variable| (name.to_owned(), variable, index)),
                ),
                Some(rest) => {
                    function = Some((name.to_owned(), indentation));
                    body = Some(rest);
                }
            }
            if body.is_none() {
                continue;
            }
        }
        match &function {
            Some((_, at)) if body.is_none() && indentation == *at && code.starts_with('}') => {
                function = None;
            }
            Some((name, _)) => functions.extend(
                assigned(body.unwrap_or(line))
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
                     every variable of sh is global, so a function names its own apart (CODE-07)"
                ),
            )
        })
        .collect()
}

/// The name of the function whose definition starts `code`, a line without its indentation:
/// `name() {`, `name () {`, `function name {` or `function name() {`, with the text after its
/// `{`; or with `None`, the same without the `{`, whose body the next line starts.
fn defined(code: &str) -> Option<(&str, Option<&str>)> {
    let (code, keyword) = match code.strip_prefix("function ") {
        Some(rest) => (rest.trim_start(), true),
        None => (code, false),
    };
    let end = code
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(code.len());
    let (name, rest) = code.split_at(end);
    let rest = rest.trim_start();
    let rest = match rest.strip_prefix("()") {
        Some(rest) => rest.trim_start(),
        None if keyword => rest,
        None => return None,
    };
    if name.is_empty() {
        return None;
    }
    match rest.strip_prefix('{') {
        Some(body) => Some((name, Some(body))),
        None => rest.is_empty().then_some((name, None)),
    }
}

/// The variables that `line` assigns: each word `name=...` where a command starts, before its
/// command word; the `name` of `for name in`; the names that `read`, `export`, `readonly`,
/// `local` and `unset` take, and the variable of `getopts`; and each `${name:=...}` or
/// `${name=...}` outside single quotes. Words are split outside quotes, and a command starts at
/// the line's start, after `;`, `&`, `|` and `(`, and after a keyword such as `then`.
fn assigned(line: &str) -> Vec<String> {
    const KEYWORDS: [&str; 10] = [
        "if", "then", "else", "elif", "do", "while", "until", "!", "{", "time",
    ];
    const ASSIGNING: [&str; 6] = ["read", "export", "readonly", "local", "unset", "getopts"];
    let variable = |name: &str| {
        name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    let mut names = Vec::new();
    let mut command = true;
    let mut after_for = false;
    let mut builtin: Option<(String, usize)> = None;
    for token in tokens(line) {
        if token.len() == 1 && ";&|()".contains(token.as_str()) {
            command = true;
            builtin = None;
            continue;
        }
        if let Some((name, arguments)) = &mut builtin {
            if !token.starts_with('-') {
                *arguments += 1;
                let assigned = token.split('=').next().unwrap_or_default();
                // getopts assigns its second argument; the others, each argument.
                if (name != "getopts" || *arguments == 2) && variable(assigned) {
                    names.push(assigned.to_owned());
                }
            }
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
        } else if ASSIGNING.contains(&token.as_str()) {
            builtin = Some((token, 0));
            command = false;
        } else if let Some((name, _)) = token.split_once('=')
            && variable(name)
        {
            names.push(name.to_owned());
        } else if !KEYWORDS.contains(&token.as_str()) {
            command = false;
        }
    }
    names.extend(expansions(line));
    names
}

/// The variables that `${name:=...}` and `${name=...}` assign in `line`, outside single quotes
/// and before a comment.
fn expansions(line: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut quoted = false;
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' => quoted = !quoted,
            b'#' if !quoted && (index == 0 || bytes[index - 1].is_ascii_whitespace()) => break,
            b'$' if !quoted && bytes.get(index + 1) == Some(&b'{') => {
                let rest = &line[index + 2..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                let (name, after) = rest.split_at(end);
                if !name.is_empty() && (after.starts_with(":=") || after.starts_with('=')) {
                    names.push(name.to_owned());
                }
            }
            _ => {}
        }
        index += 1;
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
    fn code_07_a_function_names_its_variables_apart_from_the_top_level() {
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
    fn code_07_a_function_s_builtins_and_expansions_assign_too() {
        let script = "#!/bin/sh\n\
                      state=/var/lib color=blue moment=now flag=x\n\
                      ask() {\n\
                      \x20   read -r state\n\
                      \x20   export color=green\n\
                      \x20   getopts ab flag\n\
                      \x20   : \"${moment:=later}\"\n\
                      \x20   printf '%s\\n' '${state:=quoted}' \"$color\" \"${flag:-x}\"\n\
                      \x20   read -r answer\n\
                      }\n";

        let lines: Vec<usize> = scope_problems(script)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(lines, [3, 4, 5, 6]);
    }

    #[test]
    fn code_07_a_script_is_checked_with_the_files_it_sources_in_place() {
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
    fn code_07_a_deploy_script_writes_a_value_with_printf() {
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
    fn code_07_an_echo_s_quoted_separators_and_redirections_hide_no_expansion() {
        let script = "#!/bin/sh\n\
            echo \"done; $name\"\n\
            echo \"a > $b\" | cat\n\
            echo \"x & $y\"\n\
            echo `date`\n\
            echo >&2 \"$x\"\n\
            echo 'cost: $5; not a value'\n\
            echo done > \"$log\" 2>&1\n\
            echo \\$HOME is written as it is\n\
            echo done; printf '%s\\n' \"$x\"\n";

        let problems: Vec<usize> = echo_problems(script)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [2, 3, 4, 5, 6]);
    }

    #[test]
    fn code_07_a_function_of_any_form_names_its_variables_apart_from_the_top_level() {
        let script = "#!/bin/sh\n\
                      state=/var/lib\n\
                      spaced () {\n\
                      \x20   state=1\n\
                      }\n\
                      braced()\n\
                      {\n\
                      \x20   state=2\n\
                      }\n\
                      if true; then\n\
                      \x20   nested() {\n\
                      \x20       state=3\n\
                      \x20   }\n\
                      fi\n\
                      apart() (\n\
                      \x20   state=4\n\
                      )\n";

        let lines: Vec<usize> = scope_problems(script)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(lines, [3, 7, 11]);
    }

    #[test]
    fn arch_14_a_script_s_psql_statement_names_no_table_in_any_clause_and_holds_no_expansion() {
        let script = "#!/bin/sh\n\
                      psql -qAt -c 'TRUNCATE sessions'\n\
                      psql -qAt -c \"DROP TABLE public.users\"\n\
                      psql -qAt -c 'COPY users TO STDOUT'\n\
                      psql -qAt -c \"$statement\"\n\
                      psql -qAt -c 'SELECT pg_is_in_recovery()'\n\
                      psql -qAt -c \"SELECT 'sessions', count(*) FROM pg_stat_activity\"\n";
        let tables = ["sessions".to_owned(), "users".to_owned()];

        let problems: Vec<usize> = psql_problems(script, &tables)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [2, 3, 4, 5]);
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
