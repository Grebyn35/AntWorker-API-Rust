//! `check-table-ownership`: SQL in a module names only the tables that module owns (ARCH-06).
//! A table belongs to the module whose name its creating migration holds (DATA-09), and
//! `platform`'s are those of its own migrations. Each query in a `sqlx` macro of a module of `app`
//! names only its module's tables, each of `platform` only `platform`'s, and the rest of `app`
//! runs no SQL.
//!
//! No insert of those queries gives a row its `id`, which comes from the column's default
//! (DATA-08). The check reads each `INSERT INTO <table>` of a query's text, lowercased, with
//! its strings in single quotes blanked, its `--` comments cut and its whitespace as single
//! spaces: the table's name is the run of ASCII letters, digits and `_` after it, and the
//! insert's columns are what the parentheses hold that follow that name, or `AS` and an alias
//! after it, split at commas, each without `"` around it. An insert is a finding when `id` is among them, or when neither such
//! parentheses nor `DEFAULT VALUES` follow, as when the insert lists no columns or names its
//! table's schema. The tables of [`IDS_BEFORE_INSERT`] are exempt, each a finding itself while
//! no query inserts its `id`.
//!
//! It reads the migrations and the queries as `check-tenancy` does (`tenancy`).

use std::collections::BTreeMap;

use crate::{
    Finding, Outcome,
    repo::Repo,
    rust::{self, Source},
    sql::referenced,
    tenancy::{self, sql_texts, unquoted},
};

/// The root of `platform`'s code, all of which `platform` owns.
const PLATFORM: &str = "crates/platform/src/";

/// The root of `app`'s code, whose modules own their tables.
const APP: &str = "crates/app/src/";

/// The directory of `app`'s modules.
const MODULES: &str = "crates/app/src/modules/";

/// The name of the code that owns `platform`'s tables.
const PLATFORM_OWNER: &str = "platform";

/// The tables whose rows hold a secret that is encrypted bound to the row's ID, which therefore
/// comes from the database's `uuidv7()` in a statement before the insert (SEC-02, DATA-08).
const IDS_BEFORE_INSERT: &[&str] = &["totp_factors", "totp_setups"];

/// Checks every query of `platform` and `app` against the owners of the tables it names, and
/// its inserts against DATA-08.
pub fn check(repo: &Repo) -> Outcome {
    check_with(repo, IDS_BEFORE_INSERT)
}

/// [`check`], with `ids_before_insert` as the tables whose inserts may name their `id`.
fn check_with(repo: &Repo, ids_before_insert: &[&str]) -> Outcome {
    let (owners, mut findings) = owners(repo);
    let mut ids_named = Vec::new();
    let mut checked = 0;
    for file in repo.files() {
        let code = if file.path.starts_with(PLATFORM) {
            Some(PLATFORM_OWNER)
        } else if file.path.starts_with(APP) {
            rust::feature_module(&file.path).map(|(module, _)| module)
        } else {
            continue;
        };
        if !file.path.ends_with(".rs") {
            continue;
        }
        let source = match Source::read(file) {
            Ok(source) => source,
            Err(finding) => {
                findings.push(finding);
                continue;
            }
        };
        let mut queries = Vec::new();
        sql_texts(&source.tokens, &mut queries);
        for (sql, line) in queries {
            checked += 1;
            if code.is_none() {
                findings.push(Finding::new(
                    &file.path,
                    line,
                    "code of `app` outside its modules runs no SQL: a module's repository \
                     runs it (ARCH-06)",
                ));
                continue;
            }
            let sql = unquoted(&sql.to_ascii_lowercase());
            let (allowed, refused): (Vec<_>, Vec<_>) = ids_inserted(&sql)
                .into_iter()
                .partition(|table| ids_before_insert.contains(&table.as_str()));
            ids_named.extend(allowed);
            findings.extend(
                refused
                    .iter()
                    .map(|table| id_named(&file.path, line, table)),
            );
            for table in referenced(&sql) {
                let Some(owner) = owners.get(&table) else {
                    continue;
                };
                if let Some(code) = code
                    && code != owner.as_str()
                {
                    findings.push(Finding::new(
                        &file.path,
                        line,
                        format!(
                            "a query of `{code}` names `{table}`, which `{owner}` owns: data \
                             another module owns comes through that module's functions (ARCH-06)"
                        ),
                    ));
                }
            }
        }
    }
    findings.extend(unused(ids_before_insert, &ids_named));
    Outcome {
        report: vec![format!(
            "check-table-ownership: {} tables owned, {checked} queries checked.",
            owners.len()
        )],
        findings,
    }
}

/// Each table of the migrations with the module that owns it, and a finding for each whose
/// migration's name holds no module.
fn owners(repo: &Repo) -> (BTreeMap<String, String>, Vec<Finding>) {
    let modules = modules(repo);
    let mut owners = BTreeMap::new();
    let mut findings = Vec::new();
    for (name, table) in tenancy::tables(repo) {
        match owner(&table.path, &modules) {
            Some(owner) => {
                owners.insert(name, owner);
            }
            None => findings.push(Finding::new(
                &table.path,
                table.line,
                format!(
                    "`{name}` is created by a migration whose name holds no module: name it \
                     `<timestamp>_<module>_<description>.sql` (DATA-09, ARCH-06)"
                ),
            )),
        }
    }
    (owners, findings)
}

/// The finding for an insert of `table`, at `line` of `path`, that gives its row an `id`.
fn id_named(path: &str, line: usize, table: &str) -> Finding {
    Finding::new(
        path,
        line,
        format!(
            "an `INSERT INTO {table}` names `id` among its columns, or the check reads no \
             columns of it: a row's ID comes from the column's default, with `RETURNING` \
             (DATA-08)"
        ),
    )
}

/// The finding for each of `listed`, the tables whose inserts may name their `id`, that no
/// insert of `named` names.
fn unused(listed: &[&str], named: &[String]) -> Vec<Finding> {
    let unused = listed
        .iter()
        .filter(|table| !named.iter().any(|named| named == *table));
    unused
        .map(|table| {
            Finding::new(
                "crates/xtask/src/ownership.rs",
                1,
                format!(
                    "check-table-ownership lets an insert of `{table}` name its `id`, and no \
                     query does: the list names only the tables that need it (DATA-08)"
                ),
            )
        })
        .collect()
}

/// The table of each `INSERT INTO <table>` of `sql` that gives its row an `id`, or whose
/// columns the check can't read, as the module's doc says. `sql` is lowercased, with its
/// strings blanked.
fn ids_inserted(sql: &str) -> Vec<String> {
    const INSERT: &str = "insert into ";
    let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut found = Vec::new();
    let mut rest = sql.as_str();
    while let Some(at) = rest.find(INSERT) {
        rest = &rest[at + INSERT.len()..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let after = rest[name.len()..].trim_start();
        // An alias of the table may stand before its columns.
        let after = after.strip_prefix("as ").map_or(after, |aliased| {
            aliased.trim_start_matches(word).trim_start()
        });
        let gives_id = match after.strip_prefix('(') {
            Some(columns) => columns
                .split(')')
                .next()
                .unwrap_or_default()
                .split(',')
                .any(|column| column.trim().trim_matches('"') == "id"),
            None => !after.starts_with("default values"),
        };
        if gives_id {
            found.push(name);
        }
    }
    found
}

/// The names of `app`'s modules, the directories under its `modules/`.
fn modules(repo: &Repo) -> Vec<String> {
    let mut modules: Vec<String> = repo
        .paths()
        .filter_map(|path| path.strip_prefix(MODULES))
        .filter_map(|rest| rest.split_once('/').map(|(module, _)| module.to_owned()))
        .collect();
    modules.sort();
    modules.dedup();
    modules
}

/// The module whose name the migration at `path` holds after its timestamp, the longest that
/// fits, or `platform`.
fn owner(path: &str, modules: &[String]) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let (_, rest) = name.split_once('_')?;
    modules
        .iter()
        .map(String::as_str)
        .chain([PLATFORM_OWNER])
        .filter(|module| rest.starts_with(&format!("{module}_")))
        .max_by_key(|module| module.len())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(files: &[(&str, &str)]) -> Vec<String> {
        check_with(&Repo::of(files), &[])
            .findings
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
            .collect()
    }

    const MIGRATIONS: [(&str, &str); 4] = [
        (
            "migrations/20260101000000_things_things.sql",
            "CREATE TABLE things (id uuid);",
        ),
        (
            "migrations/20260101000100_thing_tags_tags.sql",
            "CREATE TABLE tags (id uuid);",
        ),
        (
            "migrations/20260101000200_platform_jobs.sql",
            "CREATE TABLE jobs (id uuid);",
        ),
        (
            "migrations/20260101000300_misc.sql",
            "CREATE TABLE misc (id uuid);",
        ),
    ];

    #[test]
    fn arch_06_a_query_names_only_its_own_module_s_tables() {
        let mut files = MIGRATIONS.to_vec();
        files.extend([
            ("crates/app/src/modules/things/mod.rs", ""),
            ("crates/app/src/modules/thing_tags/mod.rs", ""),
            (
                "crates/app/src/modules/things/repo.rs",
                "fn a() { sqlx::query!(\"SELECT id FROM things\"); }\n\
                 fn b() { sqlx::query!(\"SELECT id FROM tags\"); }",
            ),
            (
                "crates/app/src/modules/thing_tags/repo.rs",
                "fn c() { sqlx::query_scalar!(\"INSERT INTO tags (name) VALUES ($1)\", x); }",
            ),
            (
                "crates/platform/src/jobs/repo.rs",
                "fn d() { sqlx::query!(\"UPDATE jobs SET id = id\"); }\n\
                 fn e() { sqlx::query!(\"SELECT 1 FROM things\"); }",
            ),
            (
                "crates/app/src/composition.rs",
                "fn f() { sqlx::query!(\"SELECT 1\"); }",
            ),
            (
                "crates/app/src/modules/thing_tags/service.rs",
                "fn g() { sqlx::query!(\"SELECT t.id FROM tags t, things\"); }",
            ),
        ]);

        assert_eq!(
            messages(&files),
            [
                "migrations/20260101000300_misc.sql:1: `misc` is created by a migration whose \
                 name holds no module: name it `<timestamp>_<module>_<description>.sql` \
                 (DATA-09, ARCH-06)",
                "crates/app/src/modules/things/repo.rs:2: a query of `things` names `tags`, \
                 which `thing_tags` owns: data another module owns comes through that module's \
                 functions (ARCH-06)",
                "crates/platform/src/jobs/repo.rs:2: a query of `platform` names `things`, which \
                 `things` owns: data another module owns comes through that module's functions \
                 (ARCH-06)",
                "crates/app/src/composition.rs:1: code of `app` outside its modules runs no \
                 SQL: a module's repository runs it (ARCH-06)",
                "crates/app/src/modules/thing_tags/service.rs:1: a query of `thing_tags` names \
                 `things`, which `things` owns: data another module owns comes through that \
                 module's functions (ARCH-06)",
            ]
        );
    }

    #[test]
    fn data_08_an_insert_s_table_is_found_when_it_names_id_or_its_columns_can_t_be_read() {
        let inserted = |sql: &str| ids_inserted(&unquoted(&sql.to_ascii_lowercase()));

        // An insert that names other columns, an upsert's conflict target, a string and
        // `DEFAULT VALUES` give no row its ID.
        for sql in [
            "INSERT INTO things (name, workspace_id) VALUES ($1, $2) RETURNING id",
            "INSERT INTO things (name) VALUES ('(id)') ON CONFLICT (id) DO UPDATE SET name = $1",
            "INSERT INTO things DEFAULT VALUES RETURNING id",
            "INSERT INTO things AS thing (name) VALUES ($1) ON CONFLICT (name) DO NOTHING",
            "UPDATE things SET id = id",
        ] {
            assert_eq!(inserted(sql), Vec::<String>::new(), "{sql}");
        }
        // One that names `id`, however it is written and wherever the insert stands, does.
        for sql in [
            "INSERT INTO things (id, name) VALUES ($1, $2)",
            "insert  into\n things(name ,\n \"id\" ) values ($1, $2)",
            "WITH made AS (INSERT INTO things (name, id) SELECT $1, $2 RETURNING id) SELECT 1",
        ] {
            assert_eq!(inserted(sql), ["things"], "{sql}");
        }
        // So does one whose columns the check can't read.
        assert_eq!(inserted("INSERT INTO things VALUES ($1, $2)"), ["things"]);
        assert_eq!(inserted("INSERT INTO things SELECT * FROM old"), ["things"]);
        assert_eq!(
            inserted("INSERT INTO things AS t (name, id) VALUES ($1, $2)"),
            ["things"]
        );
        assert_eq!(
            inserted("INSERT INTO things t (name) VALUES ($1)"),
            ["things"]
        );
        assert_eq!(
            inserted("INSERT INTO public.things (name) VALUES ($1)"),
            ["public"]
        );
        // Each insert of a statement is read.
        assert_eq!(
            inserted("WITH a AS (INSERT INTO x (id) VALUES ($1)) INSERT INTO y (id) VALUES ($2)"),
            ["x", "y"]
        );
    }

    #[test]
    fn data_08_an_insert_names_its_id_only_for_a_listed_table_that_some_query_inserts() {
        let mut files = MIGRATIONS.to_vec();
        files.extend([
            ("crates/app/src/modules/things/mod.rs", ""),
            (
                "crates/app/src/modules/things/repo.rs",
                "fn a() { sqlx::query!(\"INSERT INTO things (id, name) VALUES ($1, $2)\"); }",
            ),
            (
                "crates/platform/src/jobs/repo.rs",
                "fn b() { sqlx::query!(\"INSERT INTO jobs (id) VALUES ($1)\"); }",
            ),
        ]);
        let messages = |listed: &[&str]| -> Vec<String> {
            check_with(&Repo::of(&files), listed)
                .findings
                .into_iter()
                .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
                .filter(|message| message.ends_with("(DATA-08)"))
                .collect()
        };

        assert_eq!(
            messages(&["jobs", "tags"]),
            [
                "crates/app/src/modules/things/repo.rs:1: an `INSERT INTO things` names `id` \
                 among its columns, or the check reads no columns of it: a row's ID comes from \
                 the column's default, with `RETURNING` (DATA-08)",
                "crates/xtask/src/ownership.rs:1: check-table-ownership lets an insert of `tags` \
                 name its `id`, and no query does: the list names only the tables that need it \
                 (DATA-08)",
            ]
        );
    }
}
