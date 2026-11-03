//! `check-table-ownership`: SQL in a module names only the tables that module owns (ARCH-06).
//! A table belongs to the module whose name its creating migration holds (DATA-09), and
//! `platform`'s are those of its own migrations. Each query in a `sqlx` macro of a module of `app`
//! names only its module's tables, each of `platform` only `platform`'s, and the rest of `app`
//! names none.
//!
//! It reads the migrations and the queries as `check-tenancy` does (`tenancy`).

use std::collections::BTreeMap;

use crate::{
    Finding, Outcome,
    repo::Repo,
    rust::{self, Source},
    tenancy::{self, referenced, sql_texts, unquoted},
};

/// The root of `platform`'s code, all of which `platform` owns.
const PLATFORM: &str = "crates/platform/src/";

/// The root of `app`'s code, whose modules own their tables.
const APP: &str = "crates/app/src/";

/// The directory of `app`'s modules.
const MODULES: &str = "crates/app/src/modules/";

/// The name of the code that owns `platform`'s tables.
const PLATFORM_OWNER: &str = "platform";

/// Checks every query of `platform` and `app` against the owners of the tables it names.
pub fn check(repo: &Repo) -> Outcome {
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
            for table in referenced(&unquoted(&sql.to_ascii_lowercase())) {
                let Some(owner) = owners.get(&table) else {
                    continue;
                };
                if code != Some(owner.as_str()) {
                    let place = code.map_or_else(
                        || "code outside a module".to_owned(),
                        |code| format!("`{code}`"),
                    );
                    findings.push(Finding::new(
                        &file.path,
                        line,
                        format!(
                            "a query of {place} names `{table}`, which `{owner}` owns: data \
                             another module owns comes through that module's functions (ARCH-06)"
                        ),
                    ));
                }
            }
        }
    }
    Outcome {
        report: vec![format!(
            "check-table-ownership: {} tables owned, {checked} queries checked.",
            owners.len()
        )],
        findings,
    }
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
        check(&Repo::of(files))
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
                "fn c() { sqlx::query_scalar!(\"INSERT INTO tags (id) VALUES ($1)\", x); }",
            ),
            (
                "crates/platform/src/jobs/repo.rs",
                "fn d() { sqlx::query!(\"UPDATE jobs SET id = id\"); }\n\
                 fn e() { sqlx::query!(\"SELECT 1 FROM things\"); }",
            ),
            (
                "crates/app/src/composition.rs",
                "fn f() { sqlx::query!(\"DELETE FROM jobs\"); }",
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
                "crates/app/src/composition.rs:1: a query of code outside a module names `jobs`, \
                 which `platform` owns: data another module owns comes through that module's \
                 functions (ARCH-06)",
            ]
        );
    }
}
