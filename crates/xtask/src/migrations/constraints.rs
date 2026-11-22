//! DATA-10 for a constraint of a table that another migration creates: adding one in full reads
//! every row of the table under a lock that keeps the table's writes waiting, where a constraint
//! added `NOT VALID` reads none, and its validation lets writes through.
//!
//! The check reads a migration as its words, parentheses, commas, periods and semicolons
//! outside `--` comments and strings in single quotes, lowercased, and its statements as what
//! stands between semicolons outside parentheses. A statement that starts `ALTER TABLE`, then
//! `IF EXISTS` or `ONLY` or neither, names its table with the next word, or, where a period
//! follows that word, as after a schema's name, with the word after the period, and holds its
//! actions between the commas outside parentheses after it. On a table that the migration
//! doesn't create itself, as `check-tenancy` reads which it creates, an action is a finding
//! when:
//! - it starts `ADD`, then `CONSTRAINT` and a name or neither, then `FOREIGN KEY` or `CHECK`,
//!   or `ADD CONSTRAINT`, a name and `NOT NULL`, and doesn't end with `NOT VALID`;
//! - it starts `ADD`, then `CONSTRAINT` and a name or neither, then `UNIQUE` or `PRIMARY KEY`,
//!   and the words `USING INDEX` don't follow, since it builds its index in the transaction;
//! - it starts `ADD` otherwise, as an added column does, and holds `REFERENCES`, `CHECK`,
//!   `UNIQUE` or `PRIMARY KEY`, a constraint that the column brings with it;
//! - it starts `ALTER`, and holds `SET NOT NULL`;
//! - it starts `VALIDATE CONSTRAINT` and a name, and an action of the same migration starts
//!   `ADD CONSTRAINT` and that name, since the migration's one transaction would hold the
//!   lock of the addition through the validation.
//!
//! A statement that holds the words `ALTER TABLE` after its start, as a `DO` block's body does,
//! is a finding too, since the check reads no action there. A dollar-quoted body and a block
//! comment are read as statements' text.

use std::collections::BTreeSet;

use crate::{Finding, repo::File, tenancy::Table};

/// A token of a migration, with its line.
type Token = (String, usize);

/// An `ALTER TABLE`: its table, and its actions.
type Alter<'a> = (&'a str, Vec<&'a [Token]>);

/// A finding for each action of an `ALTER TABLE` in `file`, a migration, that adds or validates
/// a constraint of a table that the migration doesn't create in a form that DATA-10 refuses.
/// `tables` holds the tables of every migration, by name.
pub(super) fn check(
    file: &File,
    tables: &std::collections::BTreeMap<String, Table>,
) -> Vec<Finding> {
    let tokens = tokens(&file.text);
    let statements = split(&tokens, ";");
    let alters: Vec<Alter> = statements
        .iter()
        .filter_map(|statement| altered(statement))
        .collect();
    let added: BTreeSet<&str> = alters
        .iter()
        .flat_map(|(_, actions)| actions)
        .filter_map(|action| match words(action).as_slice() {
            ["add", "constraint", name, ..] => Some(*name),
            _ => None,
        })
        .collect();
    let mut findings = Vec::new();
    for statement in &statements {
        let inside = statement
            .windows(2)
            .skip(1)
            .find(|pair| pair[0].0 == "alter" && pair[1].0 == "table");
        if let Some(pair) = inside {
            findings.push(Finding::new(
                &file.path,
                pair[0].1,
                "check-migrations reads an `ALTER TABLE` at a statement's start: this one \
                 stands inside another statement, such as a `DO` block's, so write it as a \
                 statement of its own (DATA-10)",
            ));
        }
    }
    for (table, actions) in &alters {
        let own = tables
            .get(*table)
            .is_some_and(|created| created.view.is_none() && created.path == file.path);
        if own {
            continue;
        }
        for action in actions {
            let Some(line) = action.first().map(|(_, line)| *line) else {
                continue;
            };
            if let Some(problem) = refused(&words(action), &added) {
                findings.push(Finding::new(
                    &file.path,
                    line,
                    format!("`{table}`, which this migration doesn't create: {problem} (DATA-10)"),
                ));
            }
        }
    }
    findings
}

/// Why DATA-10 refuses the action whose words are `action`, on a table of another migration, or
/// `None`. `added` holds the names of the constraints that the migration's actions add.
fn refused(action: &[&str], added: &BTreeSet<&str>) -> Option<String> {
    let holds = |pair: [&str; 2]| action.windows(2).any(|words| words == pair);
    let kind = match action {
        ["add", "constraint", _, kind @ ..] | ["add", kind @ ..] => kind,
        ["alter", ..] if holds(["not", "null"]) && action.contains(&"set") => {
            return Some(
                "`SET NOT NULL` reads every row under a lock that keeps the table's writes \
                 waiting: add `CONSTRAINT <table>_<column>_not_null NOT NULL <column> NOT \
                 VALID`, and validate it in a later migration"
                    .to_owned(),
            );
        }
        ["validate", "constraint", name, ..] if added.contains(name) => {
            return Some(format!(
                "`{name}` is validated in the transaction that adds it, which holds the \
                 addition's lock through the validation: a later migration validates it"
            ));
        }
        _ => return None,
    };
    let named = matches!(action, ["add", "constraint", ..]);
    let not_valid = action.ends_with(&["not", "valid"]);
    match kind {
        ["foreign", "key", ..] | ["check", ..] if !not_valid => Some(
            "a constraint added in full reads every row under a lock that keeps the table's \
             writes waiting: add it `NOT VALID`, and validate it in a later migration"
                .to_owned(),
        ),
        ["not", "null", ..] if named && !not_valid => Some(
            "a `NOT NULL` added in full reads every row under a lock that keeps the table's \
             writes waiting: add it `NOT VALID`, and validate it in a later migration"
                .to_owned(),
        ),
        ["unique", ..] | ["primary", "key", ..] if !holds(["using", "index"]) => Some(
            "a constraint that builds its index in the transaction keeps the table's writes \
             waiting: build the index in a migration of its own, outside a transaction, and \
             add the constraint `USING INDEX`"
                .to_owned(),
        ),
        ["foreign", "key", ..]
        | ["check", ..]
        | ["not", "null", ..]
        | ["unique", ..]
        | ["primary", "key", ..] => None,
        _ if !named
            && (action.contains(&"references")
                || action.contains(&"check")
                || action.contains(&"unique")
                || holds(["primary", "key"])) =>
        {
            Some(
                "a column added with a constraint of its own, which the check can't hold \
                 to `NOT VALID`: add the column alone, and the constraint as an action of \
                 its own"
                    .to_owned(),
            )
        }
        _ => None,
    }
}

/// The table and the actions of `statement` when it starts `ALTER TABLE`: the word after
/// `IF EXISTS` and `ONLY`, if they stand there, and what stands between the commas outside
/// parentheses after it.
fn altered(statement: &[Token]) -> Option<Alter<'_>> {
    let mut rest = statement;
    for expected in ["alter", "table"] {
        let (first, after) = rest.split_first()?;
        if first.0 != expected {
            return None;
        }
        rest = after;
    }
    if let [first, second, after @ ..] = rest
        && first.0 == "if"
        && second.0 == "exists"
    {
        rest = after;
    }
    if let [first, after @ ..] = rest
        && first.0 == "only"
    {
        rest = after;
    }
    if let [_, period, after @ ..] = rest
        && period.0 == "."
    {
        rest = after;
    }
    let (table, actions) = rest.split_first()?;
    Some((table.0.as_str(), split(actions, ",")))
}

/// The words of `tokens`, without their parentheses and periods.
fn words(tokens: &[Token]) -> Vec<&str> {
    tokens
        .iter()
        .map(|(token, _)| token.as_str())
        .filter(|token| !matches!(*token, "(" | ")" | "."))
        .collect()
}

/// `tokens` in the parts that `separator` divides outside parentheses, without the empty ones.
fn split<'a>(tokens: &'a [Token], separator: &str) -> Vec<&'a [Token]> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0_i32, 0);
    for (at, (token, _)) in tokens.iter().enumerate() {
        depth += i32::from(token == "(") - i32::from(token == ")");
        if depth == 0 && token == separator {
            parts.push(&tokens[start..at]);
            start = at + 1;
        }
    }
    parts.push(&tokens[start..]);
    parts.retain(|part| !part.is_empty());
    parts
}

/// The words of `text`, lowercased, with its parentheses, commas, periods and semicolons, each
/// with its line, outside its `--` comments and its strings in single quotes.
fn tokens(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut quoted = false;
    for (number, line) in text.lines().enumerate() {
        let mut word = String::new();
        let mut characters = line.chars().chain(std::iter::once(' ')).peekable();
        while let Some(c) = characters.next() {
            if c == '\'' {
                quoted = !quoted;
            }
            if !quoted && c == '-' && characters.peek() == Some(&'-') {
                break;
            }
            if !quoted && (c.is_ascii_alphanumeric() || c == '_') {
                word.push(c.to_ascii_lowercase());
                continue;
            }
            if !word.is_empty() {
                tokens.push((std::mem::take(&mut word), number + 1));
            }
            if !quoted && matches!(c, '(' | ')' | ',' | '.' | ';') {
                tokens.push((c.to_string(), number + 1));
            }
        }
        if !word.is_empty() {
            tokens.push((word, number + 1));
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use crate::{migrations::check_with, repo::Repo};

    /// What the check finds in a later migration whose text is `sql`, each as its line and the
    /// start of its message.
    fn found(sql: &'static str) -> Vec<String> {
        let repo = Repo::of(&[
            (
                "migrations/20260101000000_platform_things.sql",
                "CREATE TABLE owners (id uuid PRIMARY KEY);\n\
                 CREATE TABLE things (id uuid, name text, owner_id uuid);\n",
            ),
            ("migrations/20260101000100_platform_later.sql", sql),
        ]);
        check_with(&repo, None, |_| Ok(Vec::new()))
            .findings
            .iter()
            .map(|finding| {
                let problem = finding
                    .message
                    .split_once(": ")
                    .map_or("", |(_, rest)| rest);
                let start: String = problem.chars().take(24).collect();
                format!("{}: {start}", finding.line)
            })
            .collect()
    }

    #[test]
    fn data_10_a_constraint_of_another_migration_s_table_is_added_not_valid() {
        let found = found(
            "ALTER TABLE things ADD CONSTRAINT things_owner_id_fkey\n\
             \x20   FOREIGN KEY (owner_id) REFERENCES owners (id) ON DELETE CASCADE;\n\
             ALTER TABLE things ADD CHECK (id IS NOT NULL OR name <> ';,');\n\
             ALTER TABLE ONLY things ADD CONSTRAINT things_name_not_null NOT NULL name;\n\
             ALTER TABLE IF EXISTS things ALTER COLUMN name SET NOT NULL;\n\
             ALTER TABLE things ADD CONSTRAINT things_name_key UNIQUE (name);\n\
             ALTER TABLE things ADD PRIMARY KEY (id);\n\
             ALTER TABLE things\n\
             \x20   ADD COLUMN other_id uuid REFERENCES owners (id),\n\
             \x20   ADD COLUMN size int CHECK (size > 0);\n\
             -- ALTER TABLE things ADD CHECK (in_a_comment);\n\
             ALTER TABLE public.things ADD CHECK (id IS NOT NULL OR name IS NOT NULL);\n\
             DO $$ BEGIN\n\
             \x20   ALTER TABLE things ADD CHECK (id IS NOT NULL OR name IS NOT NULL);\n\
             END $$;\n",
        );

        assert_eq!(
            found,
            [
                "14: this one stands inside a",
                "1: a constraint added in fu",
                "3: a constraint added in fu",
                "4: a `NOT NULL` added in fu",
                "5: `SET NOT NULL` reads eve",
                "6: a constraint that builds",
                "7: a constraint that builds",
                "9: a column added with a co",
                "10: a column added with a co",
                "12: a constraint added in fu",
            ]
        );
    }

    #[test]
    fn data_10_a_constraint_added_not_valid_is_validated_by_another_migration() {
        let allowed = found(
            "ALTER TABLE things\n\
             \x20   DROP CONSTRAINT things_old_check,\n\
             \x20   ADD CONSTRAINT things_id_name_check CHECK (id IS NOT NULL OR name <> '') NOT VALID,\n\
             \x20   ADD CONSTRAINT things_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES owners (id)\n\
             \x20       ON DELETE CASCADE NOT VALID,\n\
             \x20   ADD CONSTRAINT things_name_not_null NOT NULL name NOT VALID,\n\
             \x20   ADD CONSTRAINT things_name_key UNIQUE USING INDEX things_name_key,\n\
             \x20   ADD COLUMN note text NOT NULL DEFAULT '',\n\
             \x20   ALTER COLUMN name SET DEFAULT 'not null';\n\
             ALTER TABLE things VALIDATE CONSTRAINT things_earlier_check;\n\
             CREATE TABLE others (id uuid, owner_id uuid);\n\
             ALTER TABLE others ADD CONSTRAINT others_owner_id_fkey\n\
             \x20   FOREIGN KEY (owner_id) REFERENCES owners (id), ALTER COLUMN id SET NOT NULL;\n",
        );
        let validated = found(
            "ALTER TABLE things ADD CONSTRAINT things_id_name_check CHECK (id IS NOT NULL OR name <> '') NOT VALID;\n\
             ALTER TABLE things VALIDATE CONSTRAINT things_id_name_check;\n",
        );

        assert_eq!(allowed, Vec::<String>::new());
        assert_eq!(validated, ["2: `things_id_name_check` i"]);
    }
}
