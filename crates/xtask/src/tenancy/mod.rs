//! `check-tenancy`: every query of workspace data filters by its tenant itself, and every table is
//! placed (DATA-16, D39).
//! - A table that the migrations create with a `workspace_id` column holds workspace data. Each
//!   query of one in the code of `platform` and `app`, in a `sqlx` macro, names its tenant: a
//!   `workspace_id`, or for a table that also has a `user_id`, a `user_id`, compared in its
//!   `WHERE` or among the columns it inserts. A table of [`ADDRESSED`] is also reached by the
//!   address of the user its rows are addressed to, which their own queries name instead. A file
//!   of a scheduled job that spans tenants, in [`CROSS_TENANT`], is exempt, and so is a file of
//!   reads by a row's ID alone, of rows that whoever holds the ID may read, in [`PUBLIC_READS`].
//! - Every other table is listed below: as account data, keyed by a user; as platform data, whose
//!   scope columns, if any, record whom its work is for; as a module's bookkeeping of what it keeps
//!   outside the database, which holds no tenant's data; or as the workspaces themselves.
//!
//! - A view takes the columns of the table it selects from, whose rows it shows, so a view of
//!   workspace data, such as `deletable_audit_entries`, is checked as workspace data: a query of it
//!   names a tenant, or is in a file of [`CROSS_TENANT`]. Any other view is placed by its own name,
//!   as a table is.
//!
//! It reads the migrations' tables and views as `tables` does, and the code as token streams
//! (`rust`).

use std::collections::BTreeMap;

use proc_macro2::{Delimiter, TokenTree};

use crate::{Finding, Outcome, repo::Repo, rust::Source, sql::referenced};

mod tables;

pub use tables::{Table, tables};

/// The migrations, whose tables the check places.
const MIGRATIONS: &str = "migrations/";

/// The code that runs queries.
const CODE: [&str; 2] = ["crates/platform/src/", "crates/app/src/"];

/// Tables of account data, keyed by a user (DATA-16).
const ACCOUNT_DATA: &[&str] = &[
    "users",
    "identities",
    "provider_identities",
    "sessions",
    "device_tokens",
    "verification_codes",
    // A user's second factors, and the challenges of their sign-ins (the auth spec's "Second
    // factors").
    "totp_factors",
    "totp_setups",
    "recovery_codes",
    "sign_in_challenges",
    // The client of an audit entry, keyed by the user whose deletion deletes it (AUDIT-006).
    "audit_entry_clients",
    "exports",
    // The documents of a user's export, reached through its export's row, whose user they are.
    "export_entries",
    "export_chunks",
    // part: files
    "user_files",
    "avatars",
    "exported_files",
    // end part: files
    // part: superadmins
    // Who is a superadmin, each row a user's own.
    "superadmins",
    // end part: superadmins
];

/// Tables of `platform`'s own bookkeeping (DATA-16). A job records the workspace and the user it
/// runs for, which its handler filters by, but the queue's own queries select jobs by their state
/// (PLATFORM-013). An event records whom it reaches, which a stream's replay filters by, but the
/// hub loads events by their position (PLATFORM-027).
const PLATFORM_DATA: &[&str] = &[
    "rate_limit_buckets",
    "idempotency_records",
    "jobs",
    "job_schedules",
    "events",
];

/// Tables of a module's bookkeeping of what it keeps outside the database, which hold no tenant's
/// data (DATA-16).
const BOOKKEEPING: &[&str] = &[
    // part: files
    // The keys of the objects that no row of a file, an avatar or a workspace's picture names.
    "object_deletions",
    // end part: files
];

/// Tables of workspace data whose rows are addressed to a user by email, with that column: the
/// user's own queries of them filter by the caller's address (DATA-16).
const ADDRESSED: &[(&str, &str)] = &[
    // part: invitations
    ("invitations", "email"),
    // end part: invitations
];

/// The tenants themselves, whose rows a query reaches by IDs that came from a query filtered by
/// tenant (DATA-16).
// part: superadmins
/// A superadmin's list reads every one of them, under its proof, which DATA-16 exempts.
// end part: superadmins
const TENANTS: &[&str] = &["workspaces"];

/// The files of scheduled jobs that span tenants, which select rows by their own criteria, such
/// as expiry (DATA-16).
const CROSS_TENANT: &[&str] = &[
    "crates/app/src/modules/accounts/jobs.rs",
    "crates/app/src/modules/audit/jobs.rs",
    "crates/app/src/modules/auth/jobs.rs",
    // part: files
    "crates/app/src/modules/files/jobs.rs",
    // end part: files
    // part: invitations
    "crates/app/src/modules/invitations/jobs.rs",
    // end part: invitations
];

/// The files of reads by a row's ID and its visibility alone, of rows that whoever holds the ID
/// may read (DATA-16).
const PUBLIC_READS: &[&str] = &[
    // part: files
    // The reads of public files, avatars and workspaces' pictures, which whoever holds a public
    // file's ID, an avatar's or a picture's, may download.
    "crates/app/src/modules/files/repo/public.rs",
    // end part: files
];

/// The `sqlx` macros that take SQL text (DATA-14).
const MACROS: [&str; 6] = [
    "query",
    "query_as",
    "query_scalar",
    "query_unchecked",
    "query_as_unchecked",
    "query_scalar_unchecked",
];

/// Checks the migrations' tables and every query of the code.
pub fn check(repo: &Repo) -> Outcome {
    check_with(repo, ADDRESSED, CROSS_TENANT)
}

/// Checks the migrations' tables and every query of the code, with `addressed` as the tables
/// whose rows are addressed to a user, and `cross_tenant` as the files of the jobs that span
/// tenants.
fn check_with(repo: &Repo, addressed: &[(&str, &str)], cross_tenant: &[&str]) -> Outcome {
    let mut findings = Vec::new();
    let mut checked = 0;
    let tables = tables(repo);
    findings.extend(placements(&tables));
    for file in repo.files() {
        let in_code = CODE.iter().any(|code| file.path.starts_with(code));
        let exempt = cross_tenant.contains(&file.path.as_str())
            || PUBLIC_READS.contains(&file.path.as_str());
        if !file.path.ends_with(".rs") || !in_code || exempt {
            continue;
        }
        match Source::read(file) {
            Ok(source) => {
                let (count, found) = queries(&source, &tables, addressed);
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

/// Every table without a `workspace_id` that the lists don't place, and every listed name that
/// no migration creates.
fn placements(tables: &BTreeMap<String, Table>) -> Vec<Finding> {
    let listed: Vec<&str> = ACCOUNT_DATA
        .iter()
        .chain(PLATFORM_DATA)
        .chain(BOOKKEEPING)
        .chain(TENANTS)
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
                     data, platform data nor a module's bookkeeping: data a module adds for its \
                     product lives in a workspace, and a table of another kind goes in its list in \
                     crates/xtask/src/tenancy/mod.rs (DATA-16, D39)"
                ),
            )
        })
        .collect();
    for name in listed {
        if !tables.contains_key(name) {
            findings.push(Finding::new(
                "crates/xtask/src/tenancy/mod.rs",
                1,
                format!("check-tenancy lists `{name}`, which no migration creates"),
            ));
        }
    }
    findings
}

/// How many queries of `source` reach workspace data, and each that doesn't name its tenant, or
/// for a table of `addressed`, its addressee's address.
fn queries(
    source: &Source<'_>,
    tables: &BTreeMap<String, Table>,
    addressed: &[(&str, &str)],
) -> (usize, Vec<Finding>) {
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
                addressed
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
                         by its tenant itself (DATA-16)",
                        tenants.join(" or ")
                    ),
                ));
            }
        }
    }
    (checked, findings)
}

/// The SQL text of each `sqlx` macro in `tokens`, at any depth, with its line.
pub fn sql_texts(tokens: &[TokenTree], found: &mut Vec<(String, usize)>) {
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

/// Whether `sql` names one of `tenants`: among the columns an `INSERT` writes, the first statement
/// of a `WITH` too, or compared with `=` after its `WHERE`.
fn names_tenant(sql: &str, tenants: &[&str]) -> bool {
    let words = words(sql);
    let insert = match words.first().map(|(word, _)| word.as_str()) {
        Some("insert") => Some(sql),
        Some("with") => words
            .get(3)
            .filter(|(word, _)| word == "insert")
            .and_then(|_| sql.find("insert").map(|at| &sql[at..])),
        _ => None,
    };
    if let Some(sql) = insert {
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
pub fn unquoted(sql: &str) -> String {
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
    use super::{
        tables::{body_after, columns},
        *,
    };

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
             CREATE TABLE provider_identities (id uuid, user_id uuid);\n\
             CREATE TABLE sessions (id uuid);\nCREATE TABLE device_tokens (id uuid);\n\
             CREATE TABLE verification_codes (id uuid, user_id uuid);\n\
             CREATE TABLE totp_factors (id uuid, user_id uuid);\n\
             CREATE TABLE totp_setups (id uuid, user_id uuid);\n\
             CREATE TABLE recovery_codes (id uuid, user_id uuid);\n\
             CREATE TABLE sign_in_challenges (id uuid, user_id uuid);\n\
             CREATE TABLE audit_entry_clients (id uuid);\n\
             CREATE TABLE exports (id uuid, user_id uuid);\n\
             CREATE TABLE export_entries (id uuid, export_id uuid);\n\
             CREATE TABLE export_chunks (id uuid, export_id uuid);\n\
             CREATE TABLE rate_limit_buckets (id uuid);\n\
             CREATE TABLE idempotency_records (id uuid);\nCREATE TABLE workspaces (id uuid);\n\
             CREATE TABLE jobs (id uuid, workspace_id uuid, user_id uuid);\n\
             CREATE TABLE job_schedules (id uuid);\n\
             CREATE TABLE events (id uuid, workspace_id uuid, user_id uuid);\n\
             CREATE TABLE offers (id uuid, workspace_id uuid, email text);\n"
        );
        // part: files
        let migrations = migrations
            + "CREATE TABLE user_files (id uuid, user_id uuid);\n\
               CREATE TABLE avatars (id uuid, user_id uuid);\n\
               CREATE TABLE exported_files (id uuid, export_id uuid, user_id uuid);\n\
               CREATE TABLE object_deletions (id uuid, object_key text);\n";
        // end part: files
        // part: superadmins
        let migrations = migrations + "CREATE TABLE superadmins (id uuid, user_id uuid);\n";
        // end part: superadmins
        let repo = Repo::of(&[
            ("migrations/1_things.sql", &migrations),
            ("crates/app/src/modules/things/repo.rs", code),
        ]);
        check_with(&repo, &[("offers", "email")], &[])
            .findings
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn data_16_a_create_table_s_columns_are_read_past_comments_and_constraints() {
        let body = body_after(MIGRATION, 2, Some("things")).unwrap();
        assert_eq!(columns(&body), ["id", "workspace_id", "user_id", "name"]);
    }

    #[test]
    fn data_16_a_query_of_workspace_data_names_its_tenant() {
        let code = r##"
            fn f() {
                sqlx::query!("SELECT name FROM things WHERE workspace_id = $1", id);
                sqlx::query_scalar!(r#"SELECT name FROM things WHERE user_id = $1"#, id);
                sqlx::query!("INSERT INTO things (workspace_id, name) VALUES ($1, $2)", a, b);
                sqlx::query!(
                    "WITH thing AS (INSERT INTO things (user_id, name) VALUES ($1, $2) RETURNING id)
                    SELECT id FROM thing", a, b
                );
                sqlx::query!("SELECT body FROM notes WHERE id = $1", id);
            }
        "##;
        assert_eq!(
            findings(code),
            [
                "migrations/1_things.sql:10: `notes` has no `workspace_id`, and check-tenancy lists \
              it as neither account data, platform data nor a module's bookkeeping: data a module \
              adds for its product lives in a workspace, and a table of another kind goes in its \
              list in crates/xtask/src/tenancy/mod.rs (DATA-16, D39)"
            ]
        );
    }

    #[test]
    fn data_16_a_query_that_names_no_tenant_is_found() {
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
    fn data_16_a_view_holds_its_table_s_rows_so_its_queries_name_their_tenant() {
        let migrations = format!(
            "{MIGRATION}CREATE VIEW old_things AS\nSELECT id, name FROM things WHERE name <> '';\n\
             CREATE OR REPLACE VIEW other_things AS SELECT id FROM things;\n"
        );
        let code = r#"
            fn f() {
                sqlx::query!("DELETE FROM old_things WHERE id = $1", id);
            }
        "#;
        let repo = Repo::of(&[
            ("migrations/1_things.sql", &migrations),
            ("crates/app/src/modules/things/repo.rs", code),
            ("crates/app/src/modules/things/jobs.rs", code),
        ]);
        let tables = tables(&repo);
        assert_eq!(
            tables["old_things"].columns,
            ["id", "workspace_id", "user_id", "name"]
        );
        assert_eq!(tables["old_things"].line, 11);
        assert_eq!(tables["other_things"].columns, tables["things"].columns);
        let lines: Vec<String> = check_with(&repo, &[], &["crates/app/src/modules/things/jobs.rs"])
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|finding| finding.contains("names no"))
            .map(|finding| finding.split(": ").next().unwrap().to_owned())
            .collect();
        assert_eq!(lines, ["crates/app/src/modules/things/repo.rs:3"]);
    }

    #[test]
    fn data_16_a_listed_table_that_no_migration_creates_is_found() {
        let tables = BTreeMap::new();
        let listed = ACCOUNT_DATA.len() + PLATFORM_DATA.len() + BOOKKEEPING.len() + TENANTS.len();
        assert_eq!(placements(&tables).len(), listed);
    }

    #[test]
    fn data_16_platform_data_and_an_addressee_s_own_rows_name_no_workspace() {
        let code = r#"
            fn f() {
                sqlx::query!("UPDATE jobs SET attempts = attempts + 1 WHERE id = $1", id);
                sqlx::query!("SELECT id FROM offers WHERE email = $1", email);
                sqlx::query!("SELECT id FROM offers WHERE id = $1", id);
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
