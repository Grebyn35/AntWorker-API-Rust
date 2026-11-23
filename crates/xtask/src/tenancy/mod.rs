//! `check-tenancy`: every query of workspace data filters by its tenant itself, and every table is
//! placed (DATA-16, D39).
//! - A table that the migrations create with a `workspace_id` column holds workspace data, unless
//!   it is listed as platform data. The check reads a table from `CREATE TABLE <name> (`, with the
//!   columns of the list in those parentheses, and the column of each `ALTER TABLE <name> ADD
//!   COLUMN <column>`, those words one after another, so a statement adds a column as its first
//!   action. An `ALTER TABLE` is a finding when it holds a `RENAME`, or an `ADD` that none of
//!   `CONSTRAINT`, `PRIMARY`, `UNIQUE`, `CHECK`, `FOREIGN` and `EXCLUDE` follows other than that
//!   one, such as an `ADD` without `COLUMN`, or after `IF EXISTS` or `ONLY`, a statement's second
//!   action, or an `ADD COLUMN IF NOT EXISTS`. It reads a view from `CREATE VIEW <name>` or
//!   `CREATE OR REPLACE VIEW <name>`, in the tokens that `views` reads, of each line cut at its
//!   first `--`. A `CREATE` of a table or a view is a finding when it holds any of the words that
//!   `tables::MODIFIERS` lists between `CREATE` and `TABLE` or `VIEW`, such as `UNLOGGED` or
//!   `MATERIALIZED`, but for a view's `OR REPLACE`, when `IF` follows its `TABLE` or `VIEW`, when
//!   its name is no word that starts with a letter or `_`, such as a name in double quotes, or is
//!   followed by a `.`, as a schema's name is, and for a table, when no `(` follows its name, or
//!   an `AS` outside parentheses follows that `(`'s list before the statement's `;`, as one does
//!   where a query fills the table. Each query of one in the code of `platform` and `app`, in a
//!   `sqlx` macro, names its tenant: a `workspace_id`, or for a table that also has a `user_id`, a
//!   `user_id`, compared in its `WHERE` or among the columns it inserts. A macro named as one of
//!   those that `functions::MACROS` lists, `!` after it, is a finding unless it is written
//!   `sqlx::<macro>!( ... )`, with parentheses, the one form whose query the check reads. A table of [`ADDRESSED`]
//!   is also reached by the address of the user its rows are addressed to, which their own queries
//!   name instead.
//! - A query that a listed function holds is exempt: a function, by its file and its name, of a
//!   scheduled job that spans tenants, in [`CROSS_TENANT`], or of reads by a row's ID alone, of
//!   rows that whoever holds the ID may read, in [`PUBLIC_READS`]. The function that holds a
//!   query is the innermost `fn <name>` whose body holds its macro, the body being the first group
//!   in braces that follows `fn <name>` at its level, before a `;` (`functions`). So a query in a
//!   closure or an `async` block is its `fn`'s, one in a `fn` inside another `fn` is the inner
//!   one's, and one that no `fn` holds is exempt in no list. A listed function is a finding when
//!   its file holds another `fn` of its name, as two methods of one name do, or when it holds no
//!   query that the check would refuse without it.
//! - Every other table is listed below: as account data, keyed by a user; as platform data, whose
//!   scope columns, if any, record whom its work is for; as a module's bookkeeping of what it keeps
//!   outside the database, which holds no tenant's data; or as the workspaces themselves.
//!
//! - A view is workspace data when its
//!   statement names a table of workspace data, or such a view, as a query names its tables
//!   (`sql`). The check reads the statement after its `VIEW <name>`, each line cut at its first
//!   `--`, as words, runs of ASCII letters, digits, `_` and `$`, strings in single quotes, and
//!   other characters (`views`). Outside parentheses and strings, the statement ends at its first
//!   `;`, its select list runs from the `AS SELECT` right after its name to its first `FROM`, and
//!   the list's items are split at its commas. An item is a column, a word that starts with a
//!   letter or `_`, alone or after another such word and a `.`, which the view shows under that
//!   word; such a column followed by `AS` and a name, such a word, which shows the column under
//!   the name; or any other tokens followed by `AS` and a name, which show no column. A view's
//!   tenants are the names under which it shows a column `workspace_id` or `user_id`, whatever
//!   table the word before its `.` names: a query of a view of workspace data names one of them,
//!   as a query of a table names its own, or a listed function holds it, so a view that shows
//!   none, such as `deletable_audit_entries`, is queried only in one. A view is a finding when
//!   its name is followed by anything but `AS SELECT`, such as the names of its columns, when an
//!   item of its list is none of those three, such as `*`, `t.*` or an expression without `AS`,
//!   or when its statement holds a `UNION`, an `INTERSECT` or an `EXCEPT` outside parentheses.
//!   Any other view is placed by its own name, as a table is.
//!
//! It reads the migrations' tables and views as `tables` and `views` do, and the code as token
//! streams (`rust`).

use std::collections::BTreeMap;

use crate::{Finding, Outcome, repo::Repo, rust::Source, sql::referenced};

mod functions;
mod tables;
mod views;

pub use functions::sql_texts;
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
    // part: client_telemetry
    // Each user's consent to the telemetry of their clients, each row a user's own.
    "telemetry_consents",
    // end part: client_telemetry
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

/// The functions, by file and name, of scheduled jobs that span tenants, which select rows by
/// their own criteria, such as expiry (DATA-16).
const CROSS_TENANT: &[(&str, &str)] = &[
    // The entries recorded before the retention, through the view that shows no tenant.
    ("crates/app/src/modules/audit/jobs.rs", "expire"),
    // part: files
    // The objects that no row names, by their keys.
    ("crates/app/src/modules/files/jobs.rs", "find_under"),
    ("crates/app/src/modules/files/jobs.rs", "delete"),
    // end part: files
    // part: invitations
    // The expired invitations, by their expiry.
    ("crates/app/src/modules/invitations/jobs.rs", "expire"),
    // end part: invitations
];

/// The functions, by file and name, of reads by a row's ID alone, or with its visibility, of rows
/// of workspace data that whoever holds the ID may read (DATA-16).
const PUBLIC_READS: &[(&str, &str)] = &[
    // part: files
    // The reads of a public workspace file and of a workspace's picture, which whoever holds the
    // file's ID or the picture's may download.
    (
        "crates/app/src/modules/files/repo/public.rs",
        "workspace_file",
    ),
    ("crates/app/src/modules/files/repo/public.rs", "picture"),
    // end part: files
];

/// Checks the migrations' tables and every query of the code.
pub fn check(repo: &Repo) -> Outcome {
    let exempt: Vec<(&str, &str)> = CROSS_TENANT.iter().chain(PUBLIC_READS).copied().collect();
    check_with(repo, ADDRESSED, &exempt)
}

/// Checks the migrations' tables and every query of the code, with `addressed` as the tables
/// whose rows are addressed to a user, and `exempt` as the functions, by file and name, whose
/// queries are exempt.
fn check_with(repo: &Repo, addressed: &[(&str, &str)], exempt: &[(&str, &str)]) -> Outcome {
    let mut findings = Vec::new();
    let mut checked = 0;
    // Each exempt function, how many `fn` of its name its file holds, and whether it holds a query
    // that the check refuses without it.
    let mut exemptions: Vec<(&str, &str, usize, bool)> = exempt
        .iter()
        .map(|(path, name)| (*path, *name, 0, false))
        .collect();
    let tables = tables(repo);
    findings.extend(placements(&tables));
    findings.extend(tables::refused(repo));
    for file in repo.files() {
        let in_code = CODE.iter().any(|code| file.path.starts_with(code));
        if !file.path.ends_with(".rs") || !in_code {
            continue;
        }
        let held = match Source::read(file) {
            Ok(source) => functions::held(&source.tokens),
            Err(finding) => {
                findings.push(finding);
                continue;
            }
        };
        for line in &held.unread {
            findings.push(Finding::new(
                &file.path,
                *line,
                "check-tenancy reads a query of a `sqlx` macro written `sqlx::<macro>!( ... )`, \
                 and no other form, so it checks nothing of this one (DATA-16)",
            ));
        }
        for (path, name, count, _) in &mut exemptions {
            if *path == file.path {
                *count = held.functions.iter().filter(|held| held == name).count();
            }
        }
        for query in &held.queries {
            let (count, found) = refused(&file.path, query, &tables, addressed);
            let exemption = exemptions.iter_mut().find(|(path, name, ..)| {
                *path == file.path && query.function.as_deref() == Some(*name)
            });
            match exemption {
                Some((.., used)) => *used |= !found.is_empty(),
                None => {
                    checked += count;
                    findings.extend(found);
                }
            }
        }
    }
    for (path, name, count, used) in exemptions {
        let problem = match (count, used) {
            (0, _) => "which holds no `fn` of that name".to_owned(),
            (1, false) => "which holds no query there that check-tenancy refuses without the \
                           exemption: the lists name only the functions that need one"
                .to_owned(),
            (1, true) => continue,
            _ => format!("which holds {count} of that name: an exemption names one `fn`"),
        };
        findings.push(Finding::new(
            "crates/xtask/src/tenancy/mod.rs",
            1,
            format!("check-tenancy exempts the function `{name}` of {path}, {problem} (DATA-16)"),
        ));
    }
    Outcome {
        report: vec![format!(
            "check-tenancy: {} tables placed, {checked} queries of workspace data checked.",
            tables.len()
        )],
        findings,
    }
}

/// Every table without a `workspace_id`, and every view of no workspace data, that the lists
/// don't place, every view whose select list the check can't read, and every listed name that no
/// migration creates.
fn placements(tables: &BTreeMap<String, Table>) -> Vec<Finding> {
    let listed: Vec<&str> = ACCOUNT_DATA
        .iter()
        .chain(PLATFORM_DATA)
        .chain(BOOKKEEPING)
        .chain(TENANTS)
        .copied()
        .collect();
    let mut findings = Vec::new();
    for (name, table) in tables {
        let unplaced = match &table.view {
            None => !table.columns.iter().any(|column| column == "workspace_id"),
            Some(_) => !workspace_data(name, tables, 0),
        };
        let problem = if let Some(Err(problem)) = table.view.as_ref().map(|view| &view.shown) {
            format!(
                "check-tenancy can't read the select list of the view `{name}`: {problem}, so it \
                 can't tell which tenant columns the view shows (DATA-16)"
            )
        } else if unplaced && !listed.contains(&name.as_str()) {
            let kind = if table.view.is_some() {
                "reads no workspace data"
            } else {
                "has no `workspace_id`"
            };
            format!(
                "`{name}` {kind}, and check-tenancy lists it as neither account data, platform \
                 data nor a module's bookkeeping: data a module adds for its product lives in a \
                 workspace, and a table of another kind goes in its list in \
                 crates/xtask/src/tenancy/mod.rs (DATA-16, D39)"
            )
        } else {
            continue;
        };
        findings.push(Finding::new(&table.path, table.line, problem));
    }
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

/// Whether `name` is a table of workspace data, one with a `workspace_id` that isn't platform
/// data, or a view whose statement names one, or names such a view, `depth` views down.
fn workspace_data(name: &str, tables: &BTreeMap<String, Table>, depth: usize) -> bool {
    match tables.get(name).map(|table| (table, &table.view)) {
        Some((table, None)) => {
            table.columns.iter().any(|column| column == "workspace_id")
                && !PLATFORM_DATA.contains(&name)
        }
        Some((_, Some(view))) => {
            depth < tables.len()
                && view
                    .reads
                    .iter()
                    .any(|read| workspace_data(read, tables, depth + 1))
        }
        None => false,
    }
}

/// How many of the tables that `query`, of the file at `path`, names hold workspace data, and the
/// finding for each that it names no tenant of: a `workspace_id`, a table's `user_id`, a view's
/// tenants, or for a table of `addressed`, its addressee's address.
fn refused(
    path: &str,
    query: &functions::Query,
    tables: &BTreeMap<String, Table>,
    addressed: &[(&str, &str)],
) -> (usize, Vec<Finding>) {
    let mut checked = 0;
    let mut findings = Vec::new();
    let lower = unquoted(&query.sql.to_ascii_lowercase());
    for name in referenced(&lower) {
        let Some(table) = tables.get(&name) else {
            continue;
        };
        if !workspace_data(&name, tables, 0) {
            continue;
        }
        checked += 1;
        let mut tenants: Vec<&str> = match &table.view {
            None => ["workspace_id", "user_id"]
                .into_iter()
                .filter(|tenant| table.columns.iter().any(|column| column == tenant))
                .collect(),
            Some(view) => view
                .shown
                .iter()
                .flatten()
                .filter(|shown| {
                    shown
                        .source
                        .as_ref()
                        .is_some_and(|source| source == "workspace_id" || source == "user_id")
                })
                .map(|shown| shown.name.as_str())
                .collect(),
        };
        tenants.extend(
            addressed
                .iter()
                .filter(|(addressed, _)| *addressed == name)
                .map(|(_, column)| *column),
        );
        let problem = if tenants.is_empty() {
            let shows = match table.view.as_ref().map(|view| &view.shown) {
                Some(Err(_)) => "whose select list check-tenancy can't read",
                _ => "that shows no tenant column",
            };
            format!(
                "a query of `{name}`, a view of workspace data {shows}, is in no function that \
                 check-tenancy exempts: only one may query it"
            )
        } else if !names_tenant(&lower, &tenants) {
            format!(
                "a query of `{name}` names no {}: every query of workspace data filters by its \
                 tenant itself",
                tenants.join(" or ")
            )
        } else {
            continue;
        };
        findings.push(Finding::new(
            path,
            query.line,
            format!("{problem} (DATA-16)"),
        ));
    }
    (checked, findings)
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
pub fn words(text: &str) -> Vec<(String, usize)> {
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
        // part: client_telemetry
        let migrations = migrations + "CREATE TABLE telemetry_consents (id uuid, user_id uuid);\n";
        // end part: client_telemetry
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
    fn data_16_a_sqlx_macro_in_a_form_the_check_doesn_t_read_is_found() {
        let code = r#"
            use sqlx::query;
            fn f() {
                sqlx::query!("SELECT name FROM things WHERE workspace_id = $1", id);
                ::sqlx::query_scalar!("SELECT name FROM things WHERE workspace_id = $1", id);
                query!("SELECT name FROM things");
                sqlx::query_as! { "SELECT name FROM things" };
                sqlx::query![
                    "SELECT name FROM things"
                ];
                other::query!("SELECT 1");
            }
        "#;

        let found = findings(code);

        let lines: Vec<&str> = found
            .iter()
            .filter(|finding| finding.contains("and no other form"))
            .map(|finding| finding.split(": ").next().unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                "crates/app/src/modules/things/repo.rs:6",
                "crates/app/src/modules/things/repo.rs:7",
                "crates/app/src/modules/things/repo.rs:8",
                "crates/app/src/modules/things/repo.rs:11",
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
        assert_eq!(tables["old_things"].columns, ["id", "name"]);
        assert_eq!(tables["old_things"].line, 11);
        assert_eq!(tables["other_things"].columns, ["id"]);
        let exempt = [("crates/app/src/modules/things/jobs.rs", "f")];
        let found: Vec<String> = check_with(&repo, &[], &exempt)
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|finding| finding.contains("(DATA-16)") && !finding.starts_with("migrations/"))
            .collect();
        assert_eq!(
            found,
            [
                "crates/app/src/modules/things/repo.rs:3: a query of `old_things`, a view of \
                 workspace data that shows no tenant column, is in no function that check-tenancy \
                 exempts: only one may query it (DATA-16)"
            ]
        );
    }

    /// The file of the code that `exempted` checks.
    const JOBS: &str = "crates/app/src/modules/things/jobs.rs";

    /// The findings of DATA-16 of a check of `code` as the file [`JOBS`], with `views` from the
    /// third line of the migrations, and `exempt` as the exempt functions.
    fn exempted(code: &str, views: &str, exempt: &[(&str, &str)]) -> Vec<String> {
        let migrations = format!(
            "CREATE TABLE things (id uuid, workspace_id uuid, user_id uuid, name text);\n\
             CREATE TABLE jobs (id uuid, workspace_id uuid, user_id uuid);\n{views}"
        );
        let repo = Repo::of(&[("migrations/1_things.sql", &migrations), (JOBS, code)]);
        check_with(&repo, &[], exempt)
            .findings
            .iter()
            .map(ToString::to_string)
            .filter(|finding| finding.contains("DATA-16"))
            .collect()
    }

    /// The lines of [`JOBS`] that `findings` refuse.
    fn refused_lines(findings: &[String]) -> Vec<&str> {
        findings
            .iter()
            .filter_map(|finding| finding.strip_prefix(JOBS)?.strip_prefix(':'))
            .filter_map(|rest| rest.split(':').next())
            .collect()
    }

    #[test]
    fn data_16_a_query_is_exempt_where_the_innermost_fn_that_holds_it_is_listed() {
        let query = r#"sqlx::query!("DELETE FROM things WHERE id = $1", id);"#;
        let code = format!(
            "fn expire() {{
                {query}
                let later = move |context| async move {{ {query} }};
                fn helper() {{
                    {query}
                }}
            }}
            fn register() {{
                {query}
                fn expire_too() {{}}
            }}
            impl Retention {{
                fn sweep(&self) {{
                    {query}
                }}
            }}
            trait Sweeps {{
                fn expire_all();
                const QUERY: () = {{ {query} }};
            }}
            static OUTSIDE: () = {{ {query} }};
            fn pointer(f: fn() -> u8) {{
                {query}
            }}
            fn generic<const N: usize>() -> Sized<{{ N }}> {{
                {query}
            }}
            fn nested() {{
                fn expire() {{}}
            }}"
        );
        let exempt = [
            (JOBS, "expire"),
            (JOBS, "sweep"),
            (JOBS, "expire_all"),
            (JOBS, "pointer"),
            (JOBS, "generic"),
        ];
        let found = exempted(&code, "", &exempt);
        // The helper inside `expire`, `register`, the trait's constant after a declaration, the
        // static, and the body of `generic`, whose first group in braces is `{ N }`.
        assert_eq!(refused_lines(&found), ["5", "9", "19", "21", "26"]);
        let exemptions: Vec<&str> = found
            .iter()
            .filter_map(|finding| finding.strip_prefix("crates/xtask/src/tenancy/mod.rs:1: "))
            .collect();
        assert_eq!(
            exemptions,
            [
                "check-tenancy exempts the function `expire` of crates/app/src/modules/things/\
                 jobs.rs, which holds 2 of that name: an exemption names one `fn` (DATA-16)",
                "check-tenancy exempts the function `expire_all` of crates/app/src/modules/things/\
                 jobs.rs, which holds no query there that check-tenancy refuses without the \
                 exemption: the lists name only the functions that need one (DATA-16)",
                "check-tenancy exempts the function `generic` of crates/app/src/modules/things/\
                 jobs.rs, which holds no query there that check-tenancy refuses without the \
                 exemption: the lists name only the functions that need one (DATA-16)",
            ]
        );
    }

    #[test]
    fn data_16_a_listed_function_that_exempts_nothing_or_shares_its_name_is_found() {
        let code = r#"
            impl Sweep {
                fn twice(&self) {
                    sqlx::query!("DELETE FROM things WHERE id = $1", id);
                }
            }
            impl Other {
                fn twice(&self) {}
            }
            fn filtered() {
                sqlx::query!("DELETE FROM things WHERE workspace_id = $1", id);
            }
            fn needed() {
                sqlx::query!("DELETE FROM things WHERE id = $1", id);
            }
        "#;
        let other = "crates/app/src/modules/other/jobs.rs";
        let exempt = [
            (JOBS, "twice"),
            (JOBS, "filtered"),
            (JOBS, "missing"),
            (other, "needed"),
            (JOBS, "needed"),
        ];
        let found = exempted(code, "", &exempt);
        let problems: Vec<&str> = found
            .iter()
            .filter_map(|finding| finding.split(".rs, which ").nth(1))
            .collect();
        assert_eq!(
            problems,
            [
                "holds 2 of that name: an exemption names one `fn` (DATA-16)",
                "holds no query there that check-tenancy refuses without the exemption: the lists \
                 name only the functions that need one (DATA-16)",
                "holds no `fn` of that name (DATA-16)",
                "holds no `fn` of that name (DATA-16)",
            ]
        );
        assert!(refused_lines(&found).is_empty(), "{found:?}");
    }

    #[test]
    fn data_16_a_query_of_a_view_names_a_tenant_column_that_the_view_shows() {
        let views = "\
CREATE VIEW mine AS SELECT id, workspace_id AS ws, name FROM things;
CREATE VIEW swapped AS SELECT workspace_id AS id, id AS workspace_id FROM things;
CREATE VIEW bare AS SELECT id FROM things;
CREATE VIEW joined AS SELECT j.id, j.user_id FROM jobs j JOIN things t ON t.id = j.id;
CREATE VIEW of_jobs AS SELECT id, workspace_id FROM jobs;
CREATE VIEW of_bare AS SELECT id FROM bare;
";
        let code = r#"
            fn f() {
                sqlx::query!("SELECT name FROM mine WHERE ws = $1", id);
                sqlx::query!("SELECT name FROM mine WHERE workspace_id = $1", id);
                sqlx::query!("SELECT workspace_id FROM swapped WHERE id = $1", id);
                sqlx::query!("SELECT id FROM swapped WHERE workspace_id = $1", id);
                sqlx::query!(
                    "SELECT b.id FROM bare b JOIN things t ON t.id = b.id
                    WHERE t.workspace_id = $1", id
                );
                sqlx::query!("SELECT id FROM joined WHERE user_id = $1", id);
                sqlx::query!("SELECT id FROM of_bare WHERE id = $1", id);
                sqlx::query!("SELECT id FROM of_jobs WHERE id = $1", id);
            }
        "#;
        let found = exempted(code, views, &[]);
        let problems: Vec<&str> = found
            .iter()
            .filter_map(|finding| finding.strip_prefix(JOBS))
            .collect();
        assert_eq!(
            problems,
            [
                ":4: a query of `mine` names no ws: every query of workspace data filters by its \
                 tenant itself (DATA-16)",
                ":6: a query of `swapped` names no id: every query of workspace data filters by \
                 its tenant itself (DATA-16)",
                ":8: a query of `bare`, a view of workspace data that shows no tenant column, is \
                 in no function that check-tenancy exempts: only one may query it (DATA-16)",
                ":12: a query of `of_bare`, a view of workspace data that shows no tenant column, \
                 is in no function that check-tenancy exempts: only one may query it (DATA-16)",
            ]
        );
        let placed: Vec<&str> = found
            .iter()
            .filter(|finding| finding.starts_with("migrations/"))
            .map(|finding| finding.split(", and").next().unwrap())
            .collect();
        assert_eq!(
            placed,
            ["migrations/1_things.sql:7: `of_jobs` reads no workspace data"]
        );
    }

    #[test]
    fn data_16_a_view_whose_select_list_the_check_can_t_read_is_found() {
        let views = "\
CREATE VIEW starred AS SELECT * FROM things;
CREATE VIEW counted AS SELECT count(*) AS n FROM things;
";
        let code = r#"
            fn f() {
                sqlx::query!("SELECT id FROM starred WHERE workspace_id = $1", id);
                sqlx::query!("SELECT n FROM counted", id);
            }
        "#;
        let found = exempted(code, views, &[]);
        assert_eq!(
            found,
            [
                "migrations/1_things.sql:3: check-tenancy can't read the select list of the view \
                 `starred`: its item `*` is no column, as `column` or `qualifier.column`, and no \
                 expression followed by `AS name`, so it can't tell which tenant columns the view \
                 shows (DATA-16)",
                "crates/app/src/modules/things/jobs.rs:3: a query of `starred`, a view of \
                 workspace data whose select list check-tenancy can't read, is in no function \
                 that check-tenancy exempts: only one may query it (DATA-16)",
                "crates/app/src/modules/things/jobs.rs:4: a query of `counted`, a view of \
                 workspace data that shows no tenant column, is in no function that \
                 check-tenancy exempts: only one may query it (DATA-16)",
            ]
        );
    }

    #[test]
    fn data_16_a_table_or_a_view_created_in_a_form_the_check_doesn_t_read_is_found() {
        let read = "\
CREATE TABLE a (id uuid);
CREATE VIEW b AS SELECT id FROM a;
CREATE OR REPLACE VIEW c AS SELECT id FROM a;
-- CREATE TEMP TABLE in a comment, and 'CREATE TEMP TABLE' in a string.
CREATE INDEX a_id_idx ON a (id);
CREATE OR REPLACE FUNCTION rows_of() RETURNS TABLE (id uuid) LANGUAGE sql AS 'SELECT 1';
CREATE TYPE view AS ENUM ('table');
CREATE TABLE made (id uuid, n int GENERATED ALWAYS AS (1) STORED) PARTITION BY RANGE (id);
CREATE TABLE
    spread (id uuid);
";
        let unread = "\
CREATE TABLE IF NOT EXISTS d (id uuid);
CREATE UNLOGGED TABLE e (id uuid);
CREATE TEMP TABLE f (id uuid);
CREATE GLOBAL TEMPORARY TABLE g (id uuid);
CREATE MATERIALIZED VIEW h AS SELECT id FROM a;
CREATE OR REPLACE TEMP VIEW i AS SELECT id FROM a;
CREATE RECURSIVE VIEW j (id) AS SELECT id FROM a;
create materialized view if not exists k AS SELECT id FROM a;
CREATE FOREIGN TABLE l (id uuid) SERVER elsewhere;
CREATE TABLE public.m (id uuid, workspace_id uuid);
CREATE TABLE \"a\".\"b\" (id uuid);
CREATE VIEW public.n AS SELECT id FROM a;
CREATE OR REPLACE VIEW \"o\" AS SELECT id FROM a;
CREATE TABLE q AS SELECT id, workspace_id FROM a;
CREATE TABLE r (id) WITH (fillfactor = 70) AS SELECT id, workspace_id FROM a;
CREATE TEMP TABLE s AS SELECT id FROM a;
CREATE TABLE t OF typed;
CREATE TABLE u PARTITION OF a FOR VALUES IN (1);
CREATE TABLE v
    (id)
    AS SELECT id FROM a;
";
        let lines = |text| -> Vec<usize> {
            tables::unread(text)
                .into_iter()
                .map(|(line, _)| line)
                .collect()
        };
        assert_eq!(lines(read), Vec::<usize>::new());
        assert_eq!(lines(unread), (1..=19).collect::<Vec<_>>());
        let repo = Repo::of(&[
            ("migrations/1_read.sql", read),
            ("migrations/2_unread.sql", unread),
        ]);
        let tables = tables(&repo);
        assert_eq!(
            tables.keys().collect::<Vec<_>>(),
            ["a", "b", "c", "made", "spread"],
            "a statement that the check doesn't read creates nothing it reads"
        );
        assert_eq!(tables["spread"].columns, ["id"]);
        let repo = Repo::of(&[(
            "migrations/1_things.sql",
            "\nCREATE TABLE public.e (id uuid);",
        )]);
        let found: Vec<String> = tables::refused(&repo)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            found,
            [
                "migrations/1_things.sql:2: `CREATE TABLE public.e (id uuid);`: check-tenancy \
                 reads a table from `CREATE TABLE <name> (`, which no `AS` follows, and a view \
                 from `CREATE VIEW <name>` or `CREATE OR REPLACE VIEW <name>`, each name a word \
                 without quotes or a schema's name, as the doc of crates/xtask/src/tenancy/mod.rs \
                 says, so it places and checks nothing of what this statement creates (DATA-16)"
            ]
        );
    }

    #[test]
    fn data_16_a_column_that_alter_table_adds_is_its_table_s() {
        let repo = Repo::of(&[
            ("migrations/1_notes.sql", "CREATE TABLE notes (id uuid);\n"),
            (
                "migrations/2_tenant.sql",
                "ALTER TABLE notes ADD COLUMN workspace_id uuid,\n    \
                 ADD CONSTRAINT notes_body_check CHECK (true);\n\
                 ALTER TABLE notes ADD CONSTRAINT notes_id_check CHECK (true);\n",
            ),
        ]);

        assert_eq!(tables(&repo)["notes"].columns, ["id", "workspace_id"]);
    }

    #[test]
    fn data_16_a_column_added_in_a_form_the_check_doesn_t_read_is_found() {
        let read = "\
ALTER TABLE notes ADD COLUMN workspace_id uuid,
    ADD CONSTRAINT notes_body_check CHECK (true);
ALTER TABLE notes ADD CONSTRAINT notes_id_check CHECK (true), ADD UNIQUE (id);
ALTER TABLE notes VALIDATE CONSTRAINT notes_id_check;
ALTER TABLE notes
    ADD PRIMARY KEY (id);
";
        let unread = "\
ALTER TABLE notes ADD workspace_id uuid;
ALTER TABLE notes ADD COLUMN IF NOT EXISTS user_id uuid;
ALTER TABLE IF EXISTS notes ADD COLUMN user_id uuid;
ALTER TABLE ONLY notes ADD COLUMN user_id uuid;
ALTER TABLE public.notes ADD COLUMN user_id uuid;
ALTER TABLE notes DROP COLUMN body, ADD COLUMN user_id uuid;
alter table notes add column body text,
    add column user_id uuid;
ALTER TABLE notes RENAME COLUMN body TO workspace_id;
ALTER TABLE notes RENAME TO others;
";
        let lines = |text: &str| -> Vec<usize> {
            let repo = Repo::of(&[("migrations/1_notes.sql", text)]);
            tables::refused(&repo)
                .iter()
                .map(|finding| finding.line)
                .collect()
        };

        assert_eq!(lines(read), Vec::<usize>::new());
        assert_eq!(lines(unread), [1, 2, 3, 4, 5, 6, 7, 9, 10]);
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
