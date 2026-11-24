//! Each `psql` of a shell script under `deploy/`, and the statement that check-shell reads of it
//! (ARCH-14): a script reaches the database for what Postgres itself does, and its statements
//! name no table of the product schema.

use std::collections::BTreeSet;

use crate::tenancy::words;

/// Each line of `script` whose `psql` takes no statement with `-c` on that line, so a statement it
/// reads from its input would go unchecked, whose statement, the text between the quotes after
/// its ` -c `, goes on past the line, or holds a `$`, a backtick or a backslash inside double
/// quotes, whose value the check can't read, whose command could take another statement, as
/// [`another_statement`] reads it, or whose statement holds a word, outside its strings and in
/// any case, that is one of `tables` (ARCH-14).
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
            let problem = if another_statement(before, after) {
                "psql could take another statement than the one check-shell reads, through \
                 another option or past the statement's quotes: send one statement, with -c \
                 (ARCH-14)"
            } else if quote == '"' && statement.contains(['$', '`', '\\']) {
                "psql's statement holds an expansion or an escape, whose value check-shell can't \
                 read: write the statement itself after -c, in single quotes where it holds a \
                 double quote (ARCH-14)"
            } else {
                ""
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

/// Whether a `psql` could take a statement besides the one that check-shell reads, given
/// `before`, its line from `psql` to its ` -c `, and `after`, its line after the statement's
/// closing quote: when `before` holds a `;`, `|`, `&` or `)`, so the ` -c ` is another command's;
/// when `after` starts with a character other than whitespace and those four, which carries the
/// statement on, as the `\'` of `'\''` does; or when a word, split at whitespace, of `before`, or
/// of `after` up to its first of those four, holds a `$` or a backtick, whose value could be an
/// option, or starts with `--c` or `--f`, as `--command` and `--file` do, or with one `-` and
/// holds a `c` or an `f`, as `-c`, `-f` and `-qAtc` do.
fn another_statement(before: &str, after: &str) -> bool {
    let ends = |c: char| ";|&)".contains(c);
    let command = after.split(ends).next().unwrap_or_default();
    let source = |word: &str| {
        word.contains(['$', '`'])
            || word.starts_with("--c")
            || word.starts_with("--f")
            || (word.starts_with('-') && !word.starts_with("--") && word.contains(['c', 'f']))
    };
    before.contains(ends)
        || after.starts_with(|c: char| !c.is_whitespace() && !ends(c))
        || before
            .split_whitespace()
            .chain(command.split_whitespace())
            .any(source)
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
                      apsql_x=1\n";
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
}
