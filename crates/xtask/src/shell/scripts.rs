//! The shell scripts under `deploy/`, and what check-shell reads in them itself: each `echo` that
//! writes a value ([`echo_problems`]), and each function that assigns a variable of the top level
//! ([`scope_problems`]), while the module `psql` reads the statement of each `psql`. A script
//! sources a file of its own directory with a line `. "$here/<path>"`, where `here` names that
//! directory.

use crate::repo::Repo;

/// Each line of `script` whose `echo` writes a value, or whose arguments go on past the line,
/// which the check doesn't read, as [`echoed`] reads them. An `echo` is the text `echo` on a
/// line whose first character other than white space isn't `#`, with before it the line's
/// start, white space, a backslash, a quote, a backtick or one of `(`, `;`, `|`, `&` and `{`, and
/// after it the line's end, white space, a backslash, a quote, a backtick, a `<` or a `>`: so
/// the word in a string or in a comment after a command reads as one. A server's sh, such as
/// Ubuntu's dash, reads a backslash in echo's arguments as an escape, and `\c` ends the output,
/// while printf's `%s` writes the value as it is. Alpine's sh, the tests' server's, writes it as
/// it is, so no test would find one.
pub fn echo_problems(script: &str) -> Vec<(usize, &'static str)> {
    let mut problems = Vec::new();
    for (index, line) in script.lines().enumerate() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let written: Vec<Echoed> = line
            .match_indices("echo")
            .filter_map(|(start, _)| echoed(line, start))
            .collect();
        if written.contains(&Echoed::Expansion) {
            problems.push((
                index + 1,
                "echo writes a value, whose backslashes a server's sh reads as escapes; write it with printf '%s\\n' (CODE-07)",
            ));
        } else if written.contains(&Echoed::Unread) {
            problems.push((
                index + 1,
                "echo's arguments go on past its line, where check-shell reads them: end them there (CODE-07)",
            ));
        }
    }
    problems
}

/// What an `echo`'s arguments write, as [`writes_expansion`] reads them.
#[derive(PartialEq, Eq)]
enum Echoed {
    /// An expansion.
    Expansion,
    /// Text alone.
    Text,
    /// What the check doesn't read: arguments that go on past their line.
    Unread,
}

/// What the `echo` at `start` of `line` writes, or `None` where the text there is no `echo`, as
/// [`echo_problems`] reads one. Where a quote stands before it and the same quote after it, as
/// in `"echo" "$x"`, its arguments are the text after that second quote. Where a quote stands
/// before it alone, they are the text of the string that the quote opens, up to the next `'`
/// after a `'`, or after a `"` up to the next `"` that no backslash escapes: they write a value
/// where that text holds a `$` or a backtick, and go on past the line where no quote closes the
/// string. Where a backtick stands before it, they are the text up to the next backtick that no
/// backslash escapes, and go on past the line where none comes, and a backtick that a backslash
/// escapes in them starts a command substitution, which writes a value. Elsewhere they are the
/// text after the `echo`. [`writes_expansion`] reads the arguments but a string's.
fn echoed(line: &str, start: usize) -> Option<Echoed> {
    let before = line[..start].chars().next_back();
    let after = &line[start + "echo".len()..];
    let starts = before.is_none_or(|c| c.is_whitespace() || "\\\"'`(;|&{".contains(c));
    let ends = after
        .chars()
        .next()
        .is_none_or(|c| c.is_whitespace() || "\\\"'`<>".contains(c));
    if !(starts && ends) {
        return None;
    }
    Some(match before {
        Some(quote @ ('"' | '\'')) => match after.strip_prefix(quote) {
            Some(arguments) => writes_expansion(arguments),
            None => match closed(after, quote) {
                None => Echoed::Unread,
                Some(text) if text.contains(['$', '`']) => Echoed::Expansion,
                Some(_) => Echoed::Text,
            },
        },
        Some('`') => match closed(after, '`') {
            None => Echoed::Unread,
            Some(text) if text.contains("\\`") => Echoed::Expansion,
            Some(text) => writes_expansion(text),
        },
        _ => writes_expansion(after),
    })
}

/// The text of `rest` before its next `close` that no backslash escapes, where a backslash
/// escapes nothing when `close` is `'`, or `None` where `rest` holds none.
fn closed(rest: &str, close: char) -> Option<&str> {
    let mut escaped = false;
    for (at, c) in rest.char_indices() {
        if c == close && !escaped {
            return Some(&rest[..at]);
        }
        escaped = close != '\'' && c == '\\' && !escaped;
    }
    None
}

/// Whether `arguments`, an `echo`'s as [`echoed`] cuts them, hold a `$` or a backtick outside
/// single quotes, which no backslash escapes, before the command ends at a `;`, `|`, `&`, `)` or
/// a `#` that starts a word, outside quotes; or whether, with neither before they end, a quote
/// that they don't close, or a backslash that ends them, carries them on to the next line. The
/// `&` of a redirection, as in `>&2`, ends nothing, and the word after a `>` or a `<` is the
/// redirection's, no argument.
fn writes_expansion(arguments: &str) -> Echoed {
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
                if chars.next().is_none() {
                    return Echoed::Unread;
                }
            }
            Some(_) if c == '"' => quote = None,
            None if c == '\'' || c == '"' => quote = Some(c),
            _ if c == '$' || c == '`' => {
                if !redirected {
                    return Echoed::Expansion;
                }
            }
            Some(_) => {}
            None if c == '>' || c == '<' => redirected = true,
            None if c == '&' && (previous == '>' || previous == '<') => {}
            None if ";|&)".contains(c) || (c == '#' && previous.is_whitespace()) => {
                return Echoed::Text;
            }
            None if c.is_whitespace()
                && !(previous.is_whitespace() || "<>&".contains(previous)) =>
            {
                redirected = false;
            }
            None => {}
        }
        previous = c;
    }
    if quote.is_some() {
        Echoed::Unread
    } else {
        Echoed::Text
    }
}

/// The shell scripts under `deploy/`: the files named `*.sh`, and those whose shebang names a
/// shell: their first line starts with `#!` and holds a word whose last part, after any `/`, is
/// `sh`, `bash` or `dash`, as in `#!/bin/sh -e` and `#!/usr/bin/env bash`.
pub fn scripts(repo: &Repo) -> Vec<String> {
    repo.files()
        .filter(|file| file.path.starts_with("deploy/"))
        .filter(|file| {
            let shebang = file.text.lines().next().unwrap_or_default();
            file.path.ends_with(".sh")
                || shebang.strip_prefix("#!").is_some_and(|command| {
                    command.split_whitespace().any(|word| {
                        let name = word.rsplit('/').next().unwrap_or_default();
                        ["sh", "bash", "dash"].contains(&name)
                    })
                })
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
/// `${name=...}` that [`expansions`] reads. Words are split as [`tokens`] splits them, and a
/// command starts at the line's start, after `;`, `&`, `|` and `(`, and after a keyword such as
/// `then`.
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

/// The variables that `${name:=...}` and `${name=...}` assign in `line`: each whose `$` [`read`]
/// takes outside single quotes, with no backslash before it, and before the line's comment.
fn expansions(line: &str) -> Vec<String> {
    let read = read(line);
    read.windows(2)
        .filter(|pair| pair[0].1 == '$' && pair[0].2 != Taken::AsItIs && pair[1].1 == '{')
        .filter_map(|pair| {
            let rest = &line[pair[1].0 + 1..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let (name, after) = rest.split_at(end);
            (!name.is_empty() && (after.starts_with(":=") || after.starts_with('=')))
                .then(|| name.to_owned())
        })
        .collect()
}

/// The words of `line` and its separators `;`, `&`, `|`, `(` and `)`, each a token of its own,
/// split where [`read`] takes them outside quotes, before the line's comment.
fn tokens(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    for (_, c, taken) in read(line) {
        if taken == Taken::Outside && (c.is_whitespace() || ";&|()".contains(c)) {
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
            if !c.is_whitespace() {
                tokens.push(c.to_string());
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }
    tokens
}

/// How sh takes a character of a line, as [`read`] reads it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Taken {
    /// Outside quotes, with no backslash before it.
    Outside,
    /// Inside double quotes, with no backslash before it.
    InDoubleQuotes,
    /// As it is: a quote that opens or closes a string, a backslash outside single quotes and
    /// the character after it, and each character inside single quotes.
    AsItIs,
}

/// Each character of `line` before its comment, with its byte offset and how sh takes it. A `'`
/// outside quotes opens single quotes, up to the next `'`; a `"` outside quotes opens double
/// quotes, up to the next `"` that no backslash escapes; outside single quotes, a backslash
/// takes the character after it as it is; and the comment starts at a `#` outside quotes and
/// with no backslash before it, at the line's start or after white space or one of `;`, `&`,
/// `|`, `(` and `)` that stands so too.
fn read(line: &str) -> Vec<(usize, char, Taken)> {
    let mut read = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    // Whether a word starts at the character being read.
    let mut word_starts = true;
    for (at, c) in line.char_indices() {
        let taken = if escaped {
            escaped = false;
            Taken::AsItIs
        } else {
            match quote {
                Some('\'') => {
                    if c == '\'' {
                        quote = None;
                    }
                    Taken::AsItIs
                }
                Some(_) if c == '"' => {
                    quote = None;
                    Taken::AsItIs
                }
                Some(_) if c == '\\' => {
                    escaped = true;
                    Taken::AsItIs
                }
                Some(_) => Taken::InDoubleQuotes,
                None if c == '\'' || c == '"' => {
                    quote = Some(c);
                    Taken::AsItIs
                }
                None if c == '\\' => {
                    escaped = true;
                    Taken::AsItIs
                }
                None if c == '#' && word_starts => break,
                None => Taken::Outside,
            }
        };
        word_starts = taken == Taken::Outside && (c.is_whitespace() || ";&|()".contains(c));
        read.push((at, c, taken));
    }
    read
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
    fn code_07_a_function_s_assignment_is_read_as_sh_reads_quotes_backslashes_and_comments() {
        let script = "#!/bin/sh\n\
                      state=/var/lib\n\
                      ask() {\n\
                      \x20   : \"it's ${state:=a}\"\n\
                      \x20   : \"a #b\" \"${state:=a}\"\n\
                      \x20   : it\\'s ${state:=a}\n\
                      \x20   : \"a\\\"'\" ${state:=a}\n\
                      \x20   : \\#${state:=a}\n\
                      \x20   : a\\ #${state:=a}\n\
                      \x20   : '${state:=a}'\n\
                      \x20   : \"\\${state:=a}\"\n\
                      \x20   : \\${state:=a}\n\
                      \x20   :;#${state:=a}\n\
                      \x20   : x # ${state:=a}\n\
                      \x20   message=a\\ state=1\n\
                      }\n";

        let lines: Vec<usize> = scope_problems(script)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(lines, [3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn code_07_a_text_that_starts_with_a_comment_assigns_nothing() {
        assert_eq!(assigned("#; state=1 ${moment:=a}"), Vec::<String>::new());
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
    fn code_07_a_script_whose_shebang_gives_its_shell_an_option_is_read() {
        let repo = Repo::of(&[
            ("deploy/bin/strict", "#!/bin/sh -eu\ntrue\n"),
            (
                "deploy/bin/through-env",
                "#!/usr/bin/env -S bash -e\ntrue\n",
            ),
            ("deploy/bin/plain", "#!/usr/bin/env bash\ntrue\n"),
            ("deploy/bin/python", "#!/usr/bin/env python3 -u\nprint()\n"),
            ("deploy/bin/fish", "#!/usr/bin/fish\ntrue\n"),
        ]);

        let mut found = scripts(&repo);
        found.sort();

        assert_eq!(
            found,
            [
                "deploy/bin/plain",
                "deploy/bin/strict",
                "deploy/bin/through-env"
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
    fn code_07_an_echo_is_read_after_white_space_or_a_separator_on_a_line_that_is_no_comment() {
        let script = "#!/bin/sh\n\
            printf '%s\\n' \"no echo $x\"\n\
            ls  # now echo $x\n\
            v=`echo \"$y\"`\n\
            noecho $x\n\
            {echo $x; }\n\
            \x20 # echo $x\n";

        let problems: Vec<usize> = echo_problems(script)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [2, 3, 4, 6]);
    }

    #[test]
    fn code_07_an_echo_is_read_beside_a_backtick_a_backslash_a_quote_or_any_white_space() {
        let script = "#!/bin/sh\n\
            v=`echo \"$y\"`\n\
            \\echo \"$v\"\n\
            \"echo\" \"$v\"\n\
            'echo' \"$v\"\n\
            echo\t\"$v\"\n\
            echo>&2 \"$v\"\n\
            echo\\\n\
            \x20 \"$v\"\n\
            echo\n\
            \"echo\" done\n";

        let problems: Vec<usize> = echo_problems(script)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn code_07_an_echo_after_a_quote_or_a_backtick_reads_its_arguments_up_to_the_closing_one() {
        let script = "#!/bin/sh\n\
            v=`echo done` \"$x\"\n\
            sh -c 'echo \"$1\"' _ \"$x\"\n\
            sh -c 'echo done' && printf '%s\\n' \"$x\"\n\
            printf '%s\\n' \"echo done\" \"$x\"\n\
            eval \"echo \\\"$x\\\"\"\n\
            sh -c 'echo \\' \"$x\"\n\
            v=`echo \\`date\\``\n\
            sh -c 'echo; ls $x'\n\
            sh -c 'echo \"$1\"\n\
            sh -c 'echo\n\
            v=`echo done\n";

        let problems: Vec<(usize, bool)> = echo_problems(script)
            .into_iter()
            .map(|(line, problem)| (line, problem.contains("writes a value")))
            .collect();

        assert_eq!(
            problems,
            [
                (3, true),
                (6, true),
                (8, true),
                (10, false),
                (11, false),
                (12, false)
            ]
        );
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
    fn code_07_an_echo_whose_arguments_go_on_past_its_line_is_found() {
        let script = "#!/bin/sh\n\
            echo \"two\n\
            $lines\"\n\
            echo done \\\n\
            \"$x\"\n\
            echo 'one line' \"and another\"\n\
            echo 'it'\\''s'\n";

        let problems: Vec<(usize, &str)> = echo_problems(script);

        let lines: Vec<usize> = problems.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [2, 4]);
        assert!(
            problems
                .iter()
                .all(|(_, problem)| problem.contains("past its line"))
        );
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
}
