//! `check-tenancy`: every query of workspace data filters by its tenant itself, and every table is
//! placed (DATA-22, D39).
//! - A table that the migrations create with a `workspace_id` column holds workspace data. Each
//!   query of one in the code of `platform` and `app`, in a `sqlx` macro, names its tenant: a
//!   `workspace_id`, or for a table that also has a `user_id`, a `user_id`, compared in its
//!   `WHERE` or among the columns it inserts. A table of [`ADDRESSED`] is also reached by the
//!   address of the user its rows are addressed to, which their own queries name instead. A file
//!   of a scheduled job that spans tenants, in [`CROSS_TENANT`], is exempt.
//! - Every other table is listed below: as account data, keyed by a user; as platform data, whose
//!   scope columns, if any, record whom its work is for; or as the workspaces themselves.
//!
//! It reads the migrations' `CREATE TABLE` and `ALTER TABLE ... ADD COLUMN` statements as text,
//! and the code as token streams (`rust`).

use std::collections::BTreeMap;

use proc_macro2::{Delimiter, TokenTree};

use crate::{Finding, Outcome, repo::Repo, rust::Source};

/// The migrations, whose tables the check places.
const MIGRATIONS: &str = "migrations/";

/// The code that runs queries.
const CODE: [&str; 2] = ["crates/platform/src/", "crates/app/src/"];

/// Tables of account data, keyed by a user (DATA-22).
const ACCOUNT_DATA: [&str; 3] = ["users", "identities", "sessions"];

/// Tables of `platform`'s own bookkeeping (DATA-22). A job records the workspace and the user it
/// runs for, which its handler filters by, but the queue's own queries select jobs by their state
/// (PLATFORM-012).
const PLATFORM_DATA: [&str; 4] = [
    "rate_limit_buckets",
    "idempotency_records",
    "jobs",
    "job_schedules",
];

/// Tables of workspace data whose rows are addressed to a user by email, with that column: the
/// user's own queries of them filter by the caller's address (DATA-22).
const ADDRESSED: [(&str, &str); 1] = [("invitations", "email")];

/// The tenants themselves, whose rows a query reaches by IDs that came from a query filtered by
/// tenant (DATA-22).
const TENANTS: [&str; 1] = ["workspaces"];

/// The files of scheduled jobs that span tenants, which select rows by their own criteria, such
/// as expiry (DATA-22).
const CROSS_TENANT: [&str; 1] = ["crates/app/src/modules/invitations/expiry.rs"];

/// The `sqlx` macros that take SQL text (DATA-20).
const MACROS: [&str; 6] = [
    "query",
    "query_as",
    "query_scalar",
    "query_unchecked",
    "query_as_unchecked",
    "query_scalar_unchecked",
];

/// A table the migrations create, with its columns, and where it is created.
struct Table {
    columns: Vec<String>,
    path: String,
    line: usize,
}

/// Checks the migrations' tables and every query of the code.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let mut checked = 0;
    let tables = tables(repo);
    findings.extend(placements(&tables));
    for file in repo.files() {
        let in_code = CODE.iter().any(|code| file.path.starts_with(code));
        if !file.path.ends_with(".rs") || !in_code || CROSS_TENANT.contains(&file.path.as_str()) {
            continue;
        }
        match Source::read(file) {
            Ok(source) => {
                let (count, found) = queries(&source, &tables);
                checked += count;
                findings.extend(found);
            }
            Err(finding) => findings.push(finding),
        }
    }
    Outcome {
        report: vec![format!(
            "check-tenancy: {} tables placed, {checked} queries of workspace data checked.",
            tables.len()
        )],
        findings,
    }
}

/// The tables of every migration, by name.
fn tables(repo: &Repo) -> BTreeMap<String, Table> {
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
fn body_after(text: &str, line: usize, name: Option<&str>) -> Option<String> {
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
fn columns(body: &str) -> Vec<String> {
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

/// Every table without a `workspace_id` that the lists don't place, and every listed name that
/// no migration creates.
fn placements(tables: &BTreeMap<String, Table>) -> Vec<Finding> {
    let listed: Vec<&str> = ACCOUNT_DATA
        .iter()
        .chain(&PLATFORM_DATA)
        .chain(&TENANTS)
        .copied()
        .collect();
    let mut findings: Vec<Finding> = tables
        .iter()
        .filter(|(name, table)| {
            !table.columns.iter().any(|column| column == "workspace_id")
                && !listed.contains(&name.as_str())
        })
        .map(|(name, table)| {
            Finding::new(
                &table.path,
                table.line,
                format!(
                    "`{name}` has no `workspace_id`, and check-tenancy lists it as neither account \
                     nor platform data: data a module adds for its product lives in a workspace \
                     (DATA-22, D39)"
                ),
            )
        })
        .collect();
    for name in listed {
        if !tables.contains_key(name) {
            findings.push(Finding::new(
                "crates/xtask/src/tenancy.rs",
                1,
                format!("check-tenancy lists `{name}`, which no migration creates"),
            ));
        }
    }
    findings
}

/// How many queries of `source` reach workspace data, and each that doesn't name its tenant.
fn queries(source: &Source<'_>, tables: &BTreeMap<String, Table>) -> (usize, Vec<Finding>) {
    let mut found = Vec::new();
    sql_texts(&source.tokens, &mut found);
    let mut checked = 0;
    let mut findings = Vec::new();
    for (sql, line) in found {
        let lower = unquoted(&sql.to_ascii_lowercase());
        for table in referenced(&lower) {
            let Some(columns) = tables.get(&table).map(|table| &table.columns) else {
                continue;
            };
            if !columns.iter().any(|column| column == "workspace_id")
                || PLATFORM_DATA.contains(&table.as_str())
            {
                continue;
            }
            checked += 1;
            let mut tenants = vec!["workspace_id"];
            if columns.iter().any(|column| column == "user_id") {
                tenants.push("user_id");
            }
            tenants.extend(
                ADDRESSED
                    .iter()
                    .filter(|(addressed, _)| *addressed == table)
                    .map(|(_, column)| *column),
            );
            if !names_tenant(&lower, &tenants) {
                findings.push(Finding::new(
                    source.path,
                    line,
                    format!(
                        "a query of `{table}` names no {}: every query of workspace data filters \
                         by its tenant itself (DATA-22)",
                        tenants.join(" or ")
                    ),
                ));
            }
        }
    }
    (checked, findings)
}

/// The SQL text of each `sqlx` macro in `tokens`, at any depth, with its line.
fn sql_texts(tokens: &[TokenTree], found: &mut Vec<(String, usize)>) {
    for (index, token) in tokens.iter().enumerate() {
        if let TokenTree::Group(group) = token {
            let inner: Vec<TokenTree> = group.stream().into_iter().collect();
            let invoked = index >= 2
                && group.delimiter() == Delimiter::Parenthesis
                && matches!(&tokens[index - 1], TokenTree::Punct(punct) if punct.as_char() == '!')
                && matches!(&tokens[index - 2], TokenTree::Ident(name) if MACROS.contains(&name.to_string().as_str()));
            let in_sqlx = index >= 5
                && matches!(&tokens[index - 5..index - 2], [TokenTree::Ident(krate), TokenTree::Punct(_), TokenTree::Punct(_)] if *krate == "sqlx");
            if invoked && in_sqlx {
                let literal = inner.iter().find_map(|token| match token {
                    TokenTree::Literal(literal) => {
                        syn::parse_str::<syn::LitStr>(&literal.to_string())
                            .ok()
                            .map(|text| (text.value(), literal.span().start().line))
                    }
                    _ => None,
                });
                if let Some(found_text) = literal {
                    found.push(found_text);
                }
            }
            sql_texts(&inner, found);
        }
    }
}

/// The tables that `sql` reads or writes: the names after `FROM`, `JOIN`, `INTO` and `UPDATE`.
fn referenced(sql: &str) -> Vec<String> {
    let words = words(sql);
    words
        .windows(2)
        .filter(|pair| ["from", "join", "into", "update"].contains(&pair[0].0.as_str()))
        .map(|pair| pair[1].0.clone())
        .collect()
}

/// Whether `sql` names one of `tenants`: among the columns an `INSERT` writes, or compared with
/// `=` after its `WHERE`.
fn names_tenant(sql: &str, tenants: &[&str]) -> bool {
    let words = words(sql);
    if words.first().is_some_and(|(word, _)| word == "insert") {
        let columns = sql.find('(').zip(sql.find(')'));
        return columns.is_some_and(|(open, close)| {
            open < close
                && words_of(&sql[open..close])
                    .iter()
                    .any(|word| tenants.contains(&word.as_str()))
        });
    }
    let Some(at) = sql.find(" where ").or_else(|| sql.find("\nwhere ")) else {
        return false;
    };
    let filter = &sql[at..];
    tenants.iter().any(|tenant| {
        filter.match_indices(tenant).any(|(position, _)| {
            let before = filter[..position].chars().next_back();
            let after = filter[position + tenant.len()..].trim_start();
            !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') && after.starts_with('=')
        })
    })
}

/// The words of `text`, lowercased, each with its line, outside its quoted strings and comments.
fn words(text: &str) -> Vec<(String, usize)> {
    let mut words = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let code = line.split("--").next().unwrap_or_default();
        let mut quoted = false;
        let mut word = String::new();
        for c in code.chars().chain(std::iter::once(' ')) {
            if c == '\'' {
                quoted = !quoted;
            }
            if !quoted && (c.is_ascii_alphanumeric() || c == '_') {
                word.push(c.to_ascii_lowercase());
            } else if !word.is_empty() {
                words.push((std::mem::take(&mut word), number + 1));
            }
        }
    }
    words
}

/// `sql` with the text of its quoted strings blanked, so a string never reads as a column.
fn unquoted(sql: &str) -> String {
    let mut quoted = false;
    sql.chars()
        .map(|c| {
            if c == '\'' {
                quoted = !quoted;
                c
            } else if quoted {
                ' '
            } else {
                c
            }
        })
        .collect()
}

fn words_of(text: &str) -> Vec<String> {
    words(text).into_iter().map(|(word, _)| word).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIGRATION: &str = "\
-- Things.
CREATE TABLE things (
    id uuid PRIMARY KEY DEFAULT uuidv7(),
    workspace_id uuid NOT NULL,
    user_id uuid,
    -- a comment, with a comma
    name text NOT NULL,
    CONSTRAINT things_workspace_id_fkey FOREIGN KEY (workspace_id) REFERENCES workspaces (id)
);
CREATE TABLE notes (id uuid PRIMARY KEY, body text);
";

    fn findings(code: &str) -> Vec<String> {
        let migrations = format!(
            "{MIGRATION}CREATE TABLE users (id uuid);\nCREATE TABLE identities (id uuid);\n\
             CREATE TABLE sessions (id uuid);\nCREATE TABLE rate_limit_buckets (id uuid);\n\
             CREATE TABLE idempotency_records (id uuid);\nCREATE TABLE workspaces (id uuid);\n\
             CREATE TABLE jobs (id uuid, workspace_id uuid, user_id uuid);\n\
             CREATE TABLE job_schedules (id uuid);\n\
             CREATE TABLE invitations (id uuid, workspace_id uuid, email text);\n"
        );
        let repo = Repo::of(&[
            ("migrations/1_things.sql", &migrations),
            ("crates/app/src/modules/things/repo.rs", code),
        ]);
        check(&repo)
            .findings
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn data_22_a_create_table_s_columns_are_read_past_comments_and_constraints() {
        let body = body_after(MIGRATION, 2, Some("things")).unwrap();
        assert_eq!(columns(&body), ["id", "workspace_id", "user_id", "name"]);
    }

    #[test]
    fn data_22_a_query_of_workspace_data_names_its_tenant() {
        let code = r##"
            fn f() {
                sqlx::query!("SELECT name FROM things WHERE workspace_id = $1", id);
                sqlx::query_scalar!(r#"SELECT name FROM things WHERE user_id = $1"#, id);
                sqlx::query!("INSERT INTO things (workspace_id, name) VALUES ($1, $2)", a, b);
                sqlx::query!("SELECT body FROM notes WHERE id = $1", id);
            }
        "##;
        assert_eq!(
            findings(code),
            [
                "migrations/1_things.sql:10: `notes` has no `workspace_id`, and check-tenancy lists \
              it as neither account nor platform data: data a module adds for its product lives \
              in a workspace (DATA-22, D39)"
            ]
        );
    }

    #[test]
    fn data_22_a_query_that_names_no_tenant_is_found() {
        let code = r#"
            fn f() {
                let row = sqlx::query!(
                    "SELECT name FROM things WHERE id = $1",
                    id,
                );
                sqlx::query!("DELETE FROM things WHERE name = 'workspace_id = 1'");
                sqlx::query!("INSERT INTO things (name) VALUES ($1)", name);
            }
        "#;
        let found = findings(code);
        let lines: Vec<&str> = found
            .iter()
            .filter(|finding| finding.contains("names no"))
            .map(|finding| finding.split(": ").next().unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                "crates/app/src/modules/things/repo.rs:4",
                "crates/app/src/modules/things/repo.rs:7",
                "crates/app/src/modules/things/repo.rs:8",
            ]
        );
    }

    #[test]
    fn data_22_a_listed_table_that_no_migration_creates_is_found() {
        let tables = BTreeMap::new();
        assert_eq!(placements(&tables).len(), 8);
    }

    #[test]
    fn data_22_platform_data_and_an_addressee_s_own_rows_name_no_workspace() {
        let code = r#"
            fn f() {
                sqlx::query!("UPDATE jobs SET attempts = attempts + 1 WHERE id = $1", id);
                sqlx::query!("SELECT id FROM invitations WHERE email = $1", email);
                sqlx::query!("SELECT id FROM invitations WHERE id = $1", id);
            }
        "#;
        let found = findings(code);
        let lines: Vec<&str> = found
            .iter()
            .filter(|finding| finding.contains("names no"))
            .map(|finding| finding.split(": ").next().unwrap())
            .collect();
        assert_eq!(lines, ["crates/app/src/modules/things/repo.rs:5"]);
    }
}
