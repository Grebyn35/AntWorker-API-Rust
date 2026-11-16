//! The tables and views that the migrations create, with their columns, which `check-tenancy`
//! places and `check-table-ownership` gives their owners. It reads the migrations' `CREATE TABLE`,
//! `CREATE VIEW` and `ALTER TABLE ... ADD COLUMN` statements as text.

use std::collections::BTreeMap;

use super::{MIGRATIONS, words};
use crate::repo::Repo;

/// A table the migrations create, or a view, with its columns, and where it is created.
pub struct Table {
    /// Its columns' names: a view's are those of the table it selects from, whose rows it holds.
    pub columns: Vec<String>,
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
        let words = words(&file.text);
        for (index, (word, line)) in words.iter().enumerate() {
            let next = |offset: usize| words.get(index + offset).map(|(word, _)| word.as_str());
            if word == "create" && next(1) == Some("table") {
                if let (Some(name), Some(body)) = (next(2), body_after(&file.text, *line, next(2)))
                {
                    tables.insert(
                        name.to_owned(),
                        Table {
                            columns: columns(&body),
                            path: file.path.clone(),
                            line: *line,
                        },
                    );
                }
            } else if word == "create"
                && let Some(at) = [1, 3].into_iter().find(|at| next(*at) == Some("view"))
                && (at == 1 || (next(1) == Some("or") && next(2) == Some("replace")))
                && let Some(name) = next(at + 1)
            {
                // The table it selects from: the word after its first `FROM`.
                let base = words[index..]
                    .iter()
                    .position(|(word, _)| word == "from")
                    .and_then(|from| next(from + 1));
                let columns = base
                    .and_then(|base| tables.get(base))
                    .map(|table| table.columns.clone())
                    .unwrap_or_default();
                tables.insert(
                    name.to_owned(),
                    Table {
                        columns,
                        path: file.path.clone(),
                        line: *line,
                    },
                );
            } else if word == "alter" && next(1) == Some("table") && next(3) == Some("add") {
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

/// The text inside the parentheses that follow `name` on or after `line` of `text`.
pub(super) fn body_after(text: &str, line: usize, name: Option<&str>) -> Option<String> {
    let name = name?;
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
