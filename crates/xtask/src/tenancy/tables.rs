//! The tables and views that the migrations create, with their columns, which `check-tenancy`
//! places and `check-table-ownership` gives their owners. It reads the migrations' `CREATE TABLE`,
//! `CREATE VIEW` and `ALTER TABLE ... ADD COLUMN` statements as text, in the forms that the doc
//! of `tenancy` names.

use std::collections::BTreeMap;

use super::{MIGRATIONS, views::View, words};
use crate::{Finding, repo::Repo};

/// A table the migrations create, or a view, with its columns, and where it is created.
pub struct Table {
    /// Its columns' names: a view's are the names its select list shows, none where the check
    /// can't read the list.
    pub columns: Vec<String>,
    /// For a view, what its statement reads and shows.
    pub view: Option<View>,
    /// The migration that creates it.
    pub path: String,
    /// The line of its `CREATE TABLE` or `CREATE VIEW`.
    pub line: usize,
}

/// The tables and views of every migration, by name.
pub fn tables(repo: &Repo) -> BTreeMap<String, Table> {
    let mut tables = BTreeMap::new();
    let mut migrations: Vec<_> = repo
        .files()
        .filter(|file| file.path.starts_with(MIGRATIONS) && file.path.ends_with(".sql"))
        .collect();
    migrations.sort_by(|a, b| a.path.cmp(&b.path));
    for file in migrations {
        for Created { view, name, line } in created(&file.text) {
            let Ok(name) = name else { continue };
            let table = if view {
                let view = super::views::view(&file.text, line, &name);
                let columns = view.shown.iter().flatten();
                Table {
                    columns: columns.map(|shown| shown.name.clone()).collect(),
                    view: Some(view),
                    path: file.path.clone(),
                    line,
                }
            } else {
                let body = body_after(&file.text, line, &name).unwrap_or_default();
                Table {
                    columns: columns(&body),
                    view: None,
                    path: file.path.clone(),
                    line,
                }
            };
            tables.insert(name, table);
        }
        let words = words(&file.text);
        for (index, (word, _)) in words.iter().enumerate() {
            let next = |offset: usize| words.get(index + offset).map(|(word, _)| word.as_str());
            if word == "alter" && next(1) == Some("table") && next(3) == Some("add") {
                let column = if next(4) == Some("column") {
                    next(5)
                } else {
                    None
                };
                if let (Some(name), Some(column)) = (next(2), column)
                    && let Some(table) = tables.get_mut(name)
                {
                    table.columns.push(column.to_owned());
                }
            }
        }
    }
    tables
}

/// The words that Postgres takes between `CREATE` and `TABLE` or `VIEW`.
const MODIFIERS: [&str; 10] = [
    "or",
    "replace",
    "temp",
    "temporary",
    "unlogged",
    "global",
    "local",
    "foreign",
    "materialized",
    "recursive",
];

/// A `CREATE` of a table or a view.
pub(super) struct Created {
    /// Whether it creates a view.
    pub view: bool,
    /// The name it creates, where [`tables`] reads the statement, or the text of the statement's
    /// line where it doesn't.
    pub name: Result<String, String>,
    /// The line of its `CREATE`.
    pub line: usize,
}

/// Each `CREATE` of `text` that creates a table or a view, with its name where [`tables`] reads
/// the statement, in the forms that the doc of `tenancy` names.
pub(super) fn created(text: &str) -> Vec<Created> {
    let tokens: Vec<(String, usize)> = text
        .lines()
        .enumerate()
        .flat_map(|(number, line)| {
            let code = line.split("--").next().unwrap_or_default();
            super::views::tokens(code)
                .into_iter()
                .map(move |token| (token, number + 1))
        })
        .collect();
    let mut found = Vec::new();
    for (index, (token, line)) in tokens.iter().enumerate() {
        if token != "create" {
            continue;
        }
        let rest: Vec<&str> = tokens[index + 1..]
            .iter()
            .map(|(token, _)| token.as_str())
            .collect();
        let modifiers = rest
            .iter()
            .take_while(|word| MODIFIERS.contains(word))
            .count();
        let (view, mut read) = match rest.get(modifiers) {
            Some(&"table") => (false, modifiers == 0),
            Some(&"view") => (
                true,
                modifiers == 0 || rest[..modifiers] == ["or", "replace"],
            ),
            _ => continue,
        };
        let at = |offset: usize| rest.get(modifiers + offset).copied().unwrap_or_default();
        let name = at(1);
        read &= name != "if"
            && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && at(2) != ".";
        if !view && read {
            read = at(2) == "(" && !queried(&rest[modifiers + 2..]);
        }
        let statement = || {
            text.lines()
                .nth(line - 1)
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        found.push(Created {
            view,
            name: if read {
                Ok(name.to_owned())
            } else {
                Err(statement())
            },
            line: *line,
        });
    }
    found
}

/// Whether `statement`, the tokens of a `CREATE TABLE` from its name's `(` on, holds an `AS`
/// outside parentheses after that `(`'s list, before its `;`, as a table that a query fills does.
fn queried(statement: &[&str]) -> bool {
    let mut depth = 0;
    for (index, token) in statement.iter().enumerate() {
        depth += i32::from(*token == "(") - i32::from(*token == ")");
        match *token {
            ";" if depth == 0 => return false,
            "as" if depth == 0 && index > 0 => return true,
            _ => {}
        }
    }
    false
}

/// Every statement of a migration that creates a table or a view, or adds a column, in a form
/// that [`tables`] doesn't read.
pub(super) fn refused(repo: &Repo) -> Vec<Finding> {
    let mut findings = Vec::new();
    for file in repo
        .files()
        .filter(|file| file.path.starts_with(MIGRATIONS) && file.path.ends_with(".sql"))
    {
        for (line, statement) in unread(&file.text) {
            findings.push(Finding::new(&file.path, line, format!(
                "`{statement}`: check-tenancy reads a table from `CREATE TABLE <name> (`, which no \
                 `AS` follows, and a view from `CREATE VIEW <name>` or `CREATE OR REPLACE VIEW \
                 <name>`, each name a word without quotes or a schema's name, as the doc of \
                 crates/xtask/src/tenancy/mod.rs says, so it places and checks nothing of what \
                 this statement creates (DATA-16)"
            )));
        }
        for (line, statement) in unread_additions(&file.text) {
            findings.push(Finding::new(
                &file.path,
                line,
                format!(
                    "`{statement}`: check-tenancy reads the column that an `ALTER TABLE <name> ADD \
                 COLUMN <column>` adds as the statement's first action, as the doc of \
                 crates/xtask/src/tenancy/mod.rs says, and no other column that a statement adds \
                 or renames, nor a table's new name, so it places the table as it was (DATA-16)"
                ),
            ));
        }
    }
    findings
}

/// The words that an `ADD` of an `ALTER TABLE` takes before a constraint, rather than a column.
const CONSTRAINTS: [&str; 6] = [
    "constraint",
    "primary",
    "unique",
    "check",
    "foreign",
    "exclude",
];

/// The line of each `ALTER TABLE` of `text` that adds a column where [`tables`] doesn't read it,
/// or renames something, with the text of that line: a statement that holds a `RENAME`, or an
/// `ADD` that no word of [`CONSTRAINTS`] follows, but the `ADD COLUMN <column>` that follows
/// `ALTER TABLE <name>`, whose column isn't `IF`.
fn unread_additions(text: &str) -> Vec<(usize, String)> {
    let tokens: Vec<(String, usize)> = text
        .lines()
        .enumerate()
        .flat_map(|(number, line)| {
            let code = line.split("--").next().unwrap_or_default();
            super::views::tokens(code)
                .into_iter()
                .map(move |token| (token, number + 1))
        })
        .collect();
    let mut found = Vec::new();
    for (index, (token, line)) in tokens.iter().enumerate() {
        let at = |offset: usize| tokens.get(index + offset).map_or("", |(token, _)| token);
        if token != "alter" || at(1) != "table" {
            continue;
        }
        let statement = tokens[index..].iter().take_while(|(token, _)| token != ";");
        let read = |offset: usize| offset == 3 && at(4) == "column" && at(5) != "if";
        let added = statement.enumerate().any(|(offset, (token, _))| {
            token == "rename"
                || token == "add" && !CONSTRAINTS.contains(&at(offset + 1)) && !read(offset)
        });
        if added {
            let text = text.lines().nth(line - 1).unwrap_or_default();
            found.push((*line, text.trim().to_owned()));
        }
    }
    found
}

/// The line of each `CREATE` of `text` that [`created`] says [`tables`] doesn't read, with the
/// text of that line.
pub(super) fn unread(text: &str) -> Vec<(usize, String)> {
    created(text)
        .into_iter()
        .filter_map(|created| Some((created.line, created.name.err()?)))
        .collect()
}

/// The text inside the parentheses that follow `name` on or after `line` of `text`.
pub(super) fn body_after(text: &str, line: usize, name: &str) -> Option<String> {
    let start: usize = text
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum();
    let rest = &text[start..];
    let at = rest.to_ascii_lowercase().find(name)?;
    let open = rest[at..].find('(')? + at;
    let mut depth = 0;
    for (offset, c) in rest[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(rest[open + 1..open + offset].to_owned());
                }
            }
            _ => {}
        }
    }
    None
}

/// The columns of a `CREATE TABLE`'s body: the first word of each part at the top level that
/// isn't a constraint.
pub(super) fn columns(body: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut current = String::new();
    for line in body.lines() {
        // A comment holds no column.
        let line = line.split("--").next().unwrap_or_default();
        for c in line.chars().chain(std::iter::once('\n')) {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(std::mem::take(&mut current));
                    continue;
                }
                _ => {}
            }
            current.push(c);
        }
    }
    parts.push(current);
    parts
        .iter()
        .filter_map(|part| part.split_whitespace().next())
        .map(str::to_ascii_lowercase)
        .filter(|word| {
            ![
                "constraint",
                "primary",
                "unique",
                "check",
                "foreign",
                "exclude",
            ]
            .contains(&word.as_str())
        })
        .collect()
}
