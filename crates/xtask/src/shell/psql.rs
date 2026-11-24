//! Each `psql` of a shell script under `deploy/`, and the statement that check-shell reads of it
//! (ARCH-14): a script reaches the database for what Postgres itself does, and its statements
//! name no table of the product schema.

use std::collections::BTreeSet;

use crate::tenancy::words;

/// Each line of `script` whose `psql`, the text with no letter, digit or `_` on either side,
/// takes no statement with `-c` on that line, so a statement it reads from its input would go
/// unchecked, whose statement, the text between the quotes after its ` -c `, goes on past the
/// line, whose command could take another statement, or goes on past the line, as [`command`]
/// reads it, or whose statement holds a word that is one of `tables` (ARCH-14). So is each line
/// whose statement the check can't read: one that holds a `$` or a backtick inside double
/// quotes, whose value it doesn't know, a backslash, an escape inside double quotes and in single
/// quotes the start of one of psql's own commands, such as `\i`, which reads a file, or a `'` or a
/// `--` after its first `"`, `$` or `/*`, which a quoted name, a dollar quote or a comment could
/// hold. The check doesn't read a line whose first character other than whitespace is `#`, and a
/// statement's words are its runs of ASCII letters, digits and `_`, in any case, outside single
/// quotes, each `'` opening or closing them, and before the first `--` outside them.
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
                Some((
                    quote,
                    argument[1..]
                        .split_once(quote)
                        .map(|(statement, after)| (statement, &rest[..at], after)),
                ))
            });
            let Some((quote, statement)) = statement else {
                problems.push((
                    index + 1,
                    "psql takes its statement with -c, where check-shell reads it (ARCH-14)"
                        .to_owned(),
                ));
                continue;
            };
            let Some((statement, before, after)) = statement else {
                problems.push((
                    index + 1,
                    "psql's statement goes on past its line, where check-shell reads it: end it \
                     there (ARCH-14)"
                        .to_owned(),
                ));
                continue;
            };
            let problem = match command(before, after) {
                Command::Another => {
                    "psql could take another statement than the one check-shell reads, through \
                     another option or past the statement's quotes: send one statement, with -c \
                     (ARCH-14)"
                }
                Command::Unread => {
                    "psql's command goes on past its line, where check-shell reads it: end it \
                     there (ARCH-14)"
                }
                Command::One if quote == '"' && statement.contains(['$', '`', '\\']) => {
                    "psql's statement holds an expansion or an escape, whose value check-shell \
                     can't read: write the statement itself after -c, in single quotes where it \
                     holds a double quote (ARCH-14)"
                }
                Command::One if statement.contains('\\') => {
                    "psql's statement holds a backslash, which starts one of psql's own commands, \
                     such as \\i, that check-shell doesn't read: send SQL alone (ARCH-14)"
                }
                Command::One if misread(statement) => {
                    "psql's statement holds a quote or a -- after a double quote, a $ or a /*, \
                     where check-shell can't tell a name, a string or a comment as Postgres does: \
                     write it without them (ARCH-14)"
                }
                Command::One => "",
            };
            if !problem.is_empty() {
                problems.push((index + 1, problem.to_owned()));
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

/// Whether `statement` holds a `'` or a `--` after its first `"`, `$` or `/*`, where a quoted
/// name, a dollar quote or a comment could hold it, which [`words`] reads as a string's quote or
/// a comment's start.
fn misread(statement: &str) -> bool {
    ["\"", "$", "/*"]
        .iter()
        .filter_map(|mark| statement.find(mark))
        .min()
        .is_some_and(|at| statement[at..].contains('\'') || statement[at..].contains("--"))
}

/// What check-shell reads of the command of a `psql`, around its statement.
enum Command {
    /// A command that takes the one statement that check-shell reads.
    One,
    /// A command that could take another statement.
    Another,
    /// A command that goes on past its line.
    Unread,
}

/// What the command of a `psql` takes, given `before`, its line from `psql` to its ` -c `, and
/// `after`, its line after the statement's closing quote. It could take another statement when
/// `before` holds a `;`, `|`, `&`, `)` or `#`, so the ` -c ` is another command's or a comment's;
/// when `after` starts with a character other than whitespace and `;`, `|`, `&` and `)`, which
/// carries the statement on, as the `\'` of `'\''` does; when `before` ends inside quotes or
/// with a backslash; or when a word of `before` or of `after`, as [`shell_words`] reads them,
/// holds a `$`, a backtick or a `{`, whose expansion could be an option, or, as the shell passes
/// it, starts with `--c` or `--f`, as `--command` and `--file` do, or with one `-` and holds a `c`
/// or an `f`, as `-c`, `-f` and `-qAtc` do. Otherwise it goes on past its line when `after` ends
/// inside quotes, or with a backslash, before its command ends.
fn command(before: &str, after: &str) -> Command {
    let ends = |c: char| ";|&)".contains(c);
    if before.contains(|c: char| ends(c) || c == '#')
        || after.starts_with(|c: char| !c.is_whitespace() && !ends(c))
    {
        return Command::Another;
    }
    let source = |(written, passed): &(String, String)| {
        written.contains(['$', '`', '{'])
            || passed.starts_with("--c")
            || passed.starts_with("--f")
            || (passed.starts_with('-') && !passed.starts_with("--") && passed.contains(['c', 'f']))
    };
    if shell_words(before).is_none_or(|before| before.iter().any(source)) {
        return Command::Another;
    }
    match shell_words(after) {
        None => Command::Unread,
        Some(after) if after.iter().any(source) => Command::Another,
        Some(_) => Command::One,
    }
}

/// The words of `text`, split at whitespace outside quotes, up to the end of its command, each
/// as written and as the shell passes it, without its quotes and the backslashes that escape a
/// character: every one outside quotes, and inside double quotes one before a `$`, a backtick, a
/// `"` or a `\`. The command ends at a `;`, `|`, `&` or `)` outside quotes, but for a `&` after a
/// `<` or a `>`, or before a `>`, as in `2>&1` and `&>`, and at a `#` outside quotes that starts
/// a word, a comment's. `None` when `text` ends inside quotes, or with a backslash, before its
/// command ends.
fn shell_words(text: &str) -> Option<Vec<(String, String)>> {
    let mut words = Vec::new();
    let (mut written, mut passed) = (String::new(), String::new());
    let mut quote = None;
    let mut previous = ' ';
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quote.is_none() {
            let redirection = "<>".contains(previous) || chars.peek() == Some(&'>');
            if ";|)".contains(c) || (c == '&' && !redirection) || (c == '#' && written.is_empty()) {
                break;
            }
            if c.is_whitespace() {
                if !written.is_empty() {
                    words.push((std::mem::take(&mut written), std::mem::take(&mut passed)));
                }
                previous = c;
                continue;
            }
        }
        written.push(c);
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => passed.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (_, '\\') => {
                let escaped = chars.next()?;
                written.push(escaped);
                if quote.is_some() && !"$`\"\\".contains(escaped) {
                    passed.push('\\');
                }
                passed.push(escaped);
            }
            _ => passed.push(c),
        }
        previous = c;
    }
    if quote.is_some() {
        return None;
    }
    if !written.is_empty() {
        words.push((written, passed));
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                      apsql_x=1\n\
                      psql_version=1\n";
        let tables = ["sessions".to_owned(), "users".to_owned()];

        let problems: Vec<usize> = psql_problems(script, &tables)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [4, 5]);
    }

    #[test]
    fn arch_14_a_script_s_psql_statement_is_read_past_two_dashes_inside_a_string() {
        let script = "#!/bin/sh\n\
                      psql -qAt -c \"SELECT '--' FROM users\"\n\
                      psql -qAt -c \"SELECT 1 -- FROM users\"\n";
        let tables = ["users".to_owned()];

        let problems: Vec<usize> = psql_problems(script, &tables)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [2]);
    }

    #[test]
    fn arch_14_a_script_s_psql_statement_that_goes_on_past_its_line_is_found() {
        let script = "#!/bin/sh\n\
                      psql -qAt -c 'SELECT 1\n\
                      FROM users'\n\
                      psql -qAt \\\n\
                      -c 'TRUNCATE sessions'\n\
                      psql -qAt -c 'SELECT 1'\n";
        let tables = ["sessions".to_owned(), "users".to_owned()];

        let problems = psql_problems(script, &tables);

        let lines: Vec<usize> = problems.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [2, 4]);
        assert!(problems[0].1.contains("past its line"), "{problems:?}");
    }

    #[test]
    fn arch_14_a_script_s_psql_takes_no_statement_that_the_check_doesn_t_read() {
        let script = "#!/bin/sh\n\
                      psql -qAt -c \"SELECT \\\"users\\\".id\"\n\
                      psql -qAt -c 'SELECT 1' -c 'DELETE FROM users'\n\
                      psql -qAt -c 'SELECT 1' -qAtc 'DELETE FROM users'\n\
                      psql -qAt -f cleanup.sql -c 'SELECT 1'\n\
                      psql -qAt -c 'SELECT 1' --file=cleanup.sql\n\
                      psql -qAt -c 'SELECT 1' --comm 'DELETE FROM users'\n\
                      psql -qAt -c 'SELECT '\\''x'\\'' FROM users'\n\
                      psql -qAt -c 'SELECT 1' $options\n\
                      psql -qAt < cleanup.sql; grep -c 'x' log\n\
                      ready=$(psql -qAt -c 'SELECT 1' 2> /dev/null) && grep -c 'x' log\n\
                      psql -qAt -c \"SELECT ('x' || '1')::int\" 2>&1 | grep -c 1\n";
        let tables = ["users".to_owned()];

        let problems: Vec<usize> = psql_problems(script, &tables)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [2, 3, 4, 5, 6, 7, 8, 9, 10]);
    }

    #[test]
    fn arch_14_a_script_s_psql_statement_that_check_shell_can_t_read_as_postgres_does_is_found() {
        let script = "#!/bin/sh\n\
                      psql -qAt -c 'SELECT 1 AS \"--\" FROM users'\n\
                      psql -qAt -c 'SELECT $$--$$ FROM users'\n\
                      psql -qAt -c 'SELECT 1 /* -- */ FROM users'\n\
                      psql -qAt -c \"SELECT 1 /* ' */ FROM users\"\n\
                      psql -qAt -c '\\i cleanup.sql'\n\
                      psql -qAt -c 'SELECT 1 FROM \"pg_stat_activity\"'\n\
                      psql -qAt -c 'SELECT 1 -- \"x\"'\n";
        let tables = ["users".to_owned()];

        let problems: Vec<usize> = psql_problems(script, &tables)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [2, 3, 4, 5, 6]);
    }

    #[test]
    fn arch_14_a_script_s_psql_command_that_goes_on_past_its_line_is_found() {
        let script = "#!/bin/sh\n\
                      psql -qAt -c 'SELECT 1' \\\n\
                      -c 'DELETE FROM users'\n\
                      psql -qAt -c 'SELECT 1' 2>&1 \\\n\
                      -f cleanup.sql\n\
                      psql -qAt -c 'SELECT 1' -v 'x=;' \\\n\
                      -c 'DELETE FROM users'\n\
                      psql -qAt -c 'SELECT 1' -v \"x=\n\
                      \" -c 'DELETE FROM users'\n\
                      psql -qAt -c 'SELECT 1' # a comment's last character, \\\n\
                      psql -qAt -c 'SELECT 1' -v x=\\ \n";
        let tables = ["users".to_owned()];

        let problems = psql_problems(script, &tables);

        let lines: Vec<usize> = problems.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [2, 4, 6, 8]);
        assert!(
            problems
                .iter()
                .all(|(_, message)| message.contains("command goes on past its line")),
            "{problems:?}"
        );
    }

    #[test]
    fn arch_14_a_script_s_psql_option_is_read_as_the_shell_passes_it() {
        let script = "#!/bin/sh\n\
                      psql -qAt -c 'SELECT 1' '-c' 'DELETE FROM users'\n\
                      psql -qAt '-f' cleanup.sql -c 'SELECT 1'\n\
                      psql -qAt -c 'SELECT 1' 2>&1 -c 'DELETE FROM users'\n\
                      psql -qAt -c 'SELECT 1' &> log -c 'DELETE FROM users'\n\
                      psql -qAt -c 'SELECT 1' \\; -c 'DELETE FROM users'\n\
                      psql -qAt -c 'SELECT 1' -v \"x=;\" -c 'DELETE FROM users'\n\
                      psql -qAt -c 'SELECT 1' {-c,'DELETE FROM users'}\n\
                      psql -qAt # -c 'SELECT 1'\n\
                      psql -qAt -v 'x= -c 'SELECT 1'\n\
                      psql -qAt\\ -c 'SELECT 1'\n\
                      psql -qAt -c 'SELECT 1' \"\\-c\" x\n";
        let tables = ["users".to_owned()];

        let problems: Vec<usize> = psql_problems(script, &tables)
            .into_iter()
            .map(|(line, _)| line)
            .collect();

        assert_eq!(problems, [2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
    }
}
