//! `check-sql-bans`: every function, method and macro that the locked sqlx exposes and that takes
//! SQL text, outside the checked macros, is one that `clippy.toml` bans (DATA-20), so an update
//! of sqlx can't add one unbanned.
//!
//! It reads the sources of the sqlx that the workspace depends on, where `cargo metadata` locates
//! them ([`cargo`]). What sqlx exposes is in [`surface`], and what counts as taking SQL text in
//! [`signal`]. Each candidate must be reached by a ban of `disallowed-methods`, or of
//! `disallowed-macros` for a macro, resolved as clippy resolves it ([`bans`]). A ban that names a
//! crate of sqlx's but reaches nothing in it fails too: clippy only warns about it.

mod bans;
mod cargo;
mod cfg;
mod model;
mod signal;
mod surface;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use crate::{Finding, Outcome, repo::Repo};
use model::{Def, ItemId, Model, Ns};
use surface::{Callable, Kind};

/// The file that holds the bans.
const CONFIG: &str = "clippy.toml";

/// Checks the bans in `clippy.toml` against the locked sqlx crates.
pub fn check(repo: &Repo) -> Outcome {
    let Some(config) = repo.file(CONFIG) else {
        return failed(CONFIG, "clippy.toml is missing");
    };
    let model = match cargo::locked().and_then(|packages| Model::load(&packages, &read)) {
        Ok(model) => model,
        Err(message) => return failed("Cargo.lock", &message),
    };
    audit(&model, &config.text)
}

/// The findings of `config`, the text of `clippy.toml`, against `model`.
fn audit(model: &Model, config: &str) -> Outcome {
    let anchors = match signal::Anchors::find(model) {
        Ok(anchors) => anchors,
        Err(message) => return failed(CONFIG, &message),
    };
    let callables = surface::callables(model);
    let mut functions = Vec::new();
    for callable in &callables {
        if let Kind::Function(function) = &callable.kind {
            let parameters = signal::sql_parameters(model, &anchors, function);
            if !parameters.is_empty() {
                functions.push((callable, parameters));
            }
        }
    }
    let flagged: BTreeSet<Def> = functions.iter().map(|(callable, _)| callable.def).collect();
    let macros = signal::sql_macros(model, &callables, &flagged);
    let impls = surface::inherent_impls(model);
    let methods = bans::list(config, "disallowed-methods");
    let macro_bans = bans::list(config, "disallowed-macros");
    let mut findings = Vec::new();
    let banned_methods = banned(model, &impls, &methods, Ns::Value, &mut findings);
    let banned_macros = banned(model, &impls, &macro_bans, Ns::Macro, &mut findings);
    for (callable, parameters) in &functions {
        if !banned_methods.contains(&callable.def) {
            let taking = format!("in `{}`", parameters.join("`, `"));
            findings.push(unbanned(callable, &taking, &methods));
        }
    }
    let macro_callables: Vec<&Callable<'_>> = callables
        .iter()
        .filter(|callable| macros.contains(&callable.def))
        .collect();
    for callable in &macro_callables {
        if !banned_macros.contains(&callable.def) {
            findings.push(unbanned(
                callable,
                "outside the checked macros",
                &macro_bans,
            ));
        }
    }
    Outcome {
        report: vec![format!(
            "{} functions and methods, and {} macros, of the locked sqlx crates take SQL text (DATA-20)",
            functions.len(),
            macro_callables.len()
        )],
        findings,
    }
}

/// What the bans of `list` reach in `ns`, with a finding for each ban of a sqlx crate that
/// reaches nothing.
fn banned(
    model: &Model,
    impls: &BTreeMap<ItemId, Vec<ItemId>>,
    list: &bans::List,
    ns: Ns,
    findings: &mut Vec<Finding>,
) -> BTreeSet<Def> {
    let mut reached = BTreeSet::new();
    for ban in &list.bans {
        match bans::reaches(model, impls, &ban.path, ns) {
            Some(defs) if defs.is_empty() => findings.push(Finding::new(
                CONFIG,
                ban.line,
                format!(
                    "`{}` in `{}` reaches nothing in the locked sqlx crates, so it bans nothing (DATA-20)",
                    ban.path, list.key
                ),
            )),
            Some(defs) => reached.extend(defs),
            None => {}
        }
    }
    reached
}

fn unbanned(callable: &Callable<'_>, taking: &str, list: &bans::List) -> Finding {
    Finding::new(
        CONFIG,
        list.line,
        format!(
            "`{}` ({}) takes SQL text {taking}, and `{}` doesn't ban it: add its path (DATA-20)",
            callable.path, callable.location, list.key
        ),
    )
}

fn failed(path: &str, message: &str) -> Outcome {
    Outcome {
        report: Vec::new(),
        findings: vec![Finding::new(path, 1, message)],
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the checks run as a synchronous program, with no async runtime to stall"
)]
fn read(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io,
        path::{Path, PathBuf},
    };

    use super::*;
    use cargo::{Package, Packages};

    /// A small sqlx: its root re-exports from two crates of its own, by name, by rename, by glob
    /// and by a module re-exported under another name.
    const FILES: [(&str, &str); 14] = [
        (
            "/r/sqlx/src/lib.rs",
            r#"
            pub use sqlx_core::executor::{Execute, Executor};
            pub use sqlx_core::sql_str::{AssertSqlSafe, SqlSafeStr, SqlStr};
            pub use sqlx_core::query::{query, query_with_result as __query_with_result};
            pub use sqlx_core::pool::Pool;
            pub use sqlx_core::query_builder::{self, QueryBuilder};
            pub use sqlx_core::{cycle_a, Either};
            pub use sqlx_postgres::{self as postgres, PgConnection};
            pub extern crate sqlx_macros;
            #[cfg(feature = "mysql")]
            pub use sqlx_core::mysql_query;
            #[cfg(all(feature = "mysql", not(unix)))]
            pub use sqlx_mysql::MySql;
            pub mod migrate {
                pub use sqlx_core::migrate::*;
            }

            #[macro_export]
            macro_rules! query_unchecked (
                ($query:expr) => ({
                    $crate::sqlx_macros::expand_query!(source = $query, checked = false)
                })
            );

            #[macro_export]
            macro_rules! query_checked (
                ($query:expr) => ({ $crate::sqlx_macros::expand_query!(source = $query) })
            );

            #[macro_export]
            macro_rules! raw (($sql:expr) => ({ $crate::__query_with_result($sql, 0) }));
            "#,
        ),
        (
            "/r/sqlx-core/src/lib.rs",
            r#"
            pub mod executor;
            pub mod sql_str;
            pub mod query;
            mod pool_impl;
            pub mod pool {
                pub struct Pool;
            }
            pub mod query_builder;
            pub mod migrate;
            pub use either::Either;
            #[cfg(feature = "mysql")]
            pub fn mysql_query(sql: impl sql_str::SqlSafeStr) {}
            pub mod cycle_a {
                pub use crate::cycle_b::*;
            }
            pub mod cycle_b {
                pub use crate::cycle_a::*;
                pub fn listen(channel: &str) {}
            }
            "#,
        ),
        (
            "/r/sqlx-core/src/executor.rs",
            r#"
            use crate::sql_str::SqlStr;
            pub trait Execute {
                fn sql(self) -> SqlStr;
            }
            impl<T: crate::sql_str::SqlSafeStr> Execute for T {
                fn sql(self) -> SqlStr { self.into_sql_str() }
            }
            pub trait Executor {
                fn execute<E>(self, query: E) where E: Execute;
                fn prepare(self, query: SqlStr);
                fn close(self);
            }
            "#,
        ),
        (
            "/r/sqlx-core/src/sql_str.rs",
            r#"
            pub trait SqlSafeStr {
                fn into_sql_str(self) -> SqlStr;
            }
            pub struct AssertSqlSafe<T>(pub T);
            pub struct SqlStr(String);
            impl SqlStr {
                pub const fn from_static(sql: &'static str) -> Self { SqlStr(String::new()) }
                pub fn as_str(&self) -> &str { &self.0 }
            }
            "#,
        ),
        (
            "/r/sqlx-core/src/query.rs",
            r#"
            use crate::sql_str::SqlSafeStr;
            pub struct Query;
            pub fn query(sql: impl SqlSafeStr) -> Query { Query }
            pub fn query_with_result(sql: impl SqlSafeStr, arguments: u8) -> Query { Query }
            pub(crate) fn query_internal(sql: impl SqlSafeStr) {}
            "#,
        ),
        (
            "/r/sqlx-core/src/pool_impl.rs",
            r#"
            use crate::{pool::Pool, sql_str::SqlSafeStr};
            impl Pool {
                pub fn begin_with(&self, statement: impl SqlSafeStr) {}
                pub fn connect(url: &str) -> Self { Pool }
                pub(crate) fn begin_hidden(&self, statement: impl SqlSafeStr) {}
            }
            "#,
        ),
        (
            "/r/sqlx-core/src/query_builder.rs",
            r#"
            use std::fmt::Display;
            pub struct QueryBuilder;
            impl QueryBuilder {
                pub fn new(init: impl Into<String>) -> Self { QueryBuilder }
                pub fn push(&mut self, sql: impl Display) -> &mut Self { self }
                pub fn separated<Sep>(&mut self, separator: Sep) where Sep: Display {}
                pub fn push_bind(&mut self, value: u32) -> &mut Self { self }
            }
            "#,
        ),
        (
            "/r/sqlx-core/src/migrate/mod.rs",
            r#"
            mod migration;
            pub use migration::Migration;
            pub fn checksum(sql: &str) -> Vec<u8> { Vec::new() }
            "#,
        ),
        (
            "/r/sqlx-core/src/migrate/migration.rs",
            r#"
            use crate::sql_str::SqlStr;
            pub struct Migration;
            impl Migration {
                pub fn new(sql: SqlStr) -> Self { Migration }
                pub(crate) fn with_checksum(sql: SqlStr) -> Self { Migration }
            }
            "#,
        ),
        (
            "/r/sqlx-postgres/src/lib.rs",
            r#"
            mod connection;
            mod copy;
            pub use connection::PgConnection;
            pub use copy::PgPoolCopyExt;
            "#,
        ),
        (
            "/r/sqlx-postgres/src/connection.rs",
            "pub struct PgConnection;",
        ),
        (
            "/r/sqlx-postgres/src/copy.rs",
            r#"
            use crate::connection::PgConnection;
            impl PgConnection {
                pub async fn copy_in_raw(&mut self, statement: &str) {}
            }
            pub trait PgPoolCopyExt {
                fn copy_in_raw<'a>(&'a self, statement: &'a str);
            }
            "#,
        ),
        (
            "/r/sqlx-macros/src/lib.rs",
            r#"
            #[proc_macro]
            pub fn expand_query(input: TokenStream) -> TokenStream { input }
            "#,
        ),
        (
            "/r/sqlx-mysql/src/lib.rs",
            "this isn't Rust, and nothing reads it",
        ),
    ];

    /// What each function and macro of the small sqlx that takes SQL text is.
    const TAKING_SQL: [&str; 13] = [
        "sqlx::Executor::execute",
        "sqlx::Executor::prepare",
        "sqlx::query",
        "sqlx::__query_with_result",
        "sqlx::Pool::begin_with",
        "sqlx::QueryBuilder::new",
        "sqlx::QueryBuilder::push",
        "sqlx::QueryBuilder::separated",
        "sqlx::migrate::Migration::new",
        "sqlx::PgConnection::copy_in_raw",
        "sqlx::postgres::PgPoolCopyExt::copy_in_raw",
        "sqlx::query_unchecked",
        "sqlx::raw",
    ];

    fn package(name: &str, features: &[&str], dependencies: &[(&str, &str)]) -> Package {
        Package {
            name: name.to_owned(),
            directory: PathBuf::from(format!("/r/{name}")),
            root: PathBuf::from(format!("/r/{name}/src/lib.rs")),
            proc_macro: name == "sqlx-macros",
            features: features.iter().map(|&feature| feature.to_owned()).collect(),
            dependencies: dependencies
                .iter()
                .map(|&(crate_name, id)| (crate_name.to_owned(), id.to_owned()))
                .collect(),
        }
    }

    fn model() -> Model {
        let packages = Packages {
            packages: BTreeMap::from([
                (
                    "sqlx".to_owned(),
                    package(
                        "sqlx",
                        &["postgres"],
                        &[
                            ("sqlx_core", "core"),
                            ("sqlx_postgres", "postgres"),
                            ("sqlx_macros", "macros"),
                            ("sqlx_mysql", "mysql"),
                        ],
                    ),
                ),
                (
                    "core".to_owned(),
                    package("sqlx-core", &[], &[("either", "either")]),
                ),
                (
                    "postgres".to_owned(),
                    package("sqlx-postgres", &[], &[("sqlx_core", "core")]),
                ),
                ("macros".to_owned(), package("sqlx-macros", &[], &[])),
                ("mysql".to_owned(), package("sqlx-mysql", &[], &[])),
            ]),
            sqlx: "sqlx".to_owned(),
        };
        let files: BTreeMap<PathBuf, &str> = FILES
            .iter()
            .map(|&(path, text)| (PathBuf::from(path), text))
            .collect();
        let read = |path: &Path| {
            files
                .get(path)
                .map(|text| (*text).to_owned())
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        };
        Model::load(&packages, &read).unwrap()
    }

    /// The path each finding names first, with the finding's line.
    fn found(config: &str) -> Vec<(String, usize)> {
        let mut found: Vec<(String, usize)> = audit(&model(), config)
            .findings
            .iter()
            .map(|finding| {
                let path = finding.message.split('`').nth(1).unwrap_or_default();
                (path.to_owned(), finding.line)
            })
            .collect();
        found.sort();
        found
    }

    /// A `clippy.toml` that bans `methods` and `macros`.
    fn config(methods: &[&str], macros: &[&str]) -> String {
        let list = |paths: &[&str]| {
            paths
                .iter()
                .map(|path| format!("    {{ path = \"{path}\", reason = \"r\" }},\n"))
                .collect::<String>()
        };
        format!(
            "disallowed-methods = [\n{}]\ndisallowed-macros = [\n{}]\n",
            list(methods),
            list(macros)
        )
    }

    #[test]
    fn data_20_every_public_function_method_and_macro_that_takes_sql_text_needs_a_ban() {
        let methods_line = 1;
        let macros_line = 3;
        let mut expected: Vec<(String, usize)> = TAKING_SQL
            .iter()
            .map(|path| {
                let line = if ["sqlx::query_unchecked", "sqlx::raw"].contains(path) {
                    macros_line
                } else {
                    methods_line
                };
                ((*path).to_owned(), line)
            })
            .collect();
        expected.sort();

        assert_eq!(found(&config(&[], &[])), expected);
    }

    #[test]
    fn data_20_a_ban_covers_what_it_reaches_by_any_path() {
        let methods = [
            "sqlx_core::executor::Executor::execute",
            "sqlx::Executor::prepare",
            "sqlx_core::query::query",
            "sqlx::__query_with_result",
            "sqlx::Pool::begin_with",
            "sqlx::query_builder::QueryBuilder::new",
            "sqlx::QueryBuilder::push",
            "sqlx::QueryBuilder::separated",
            "sqlx::migrate::Migration::new",
            "sqlx::postgres::PgConnection::copy_in_raw",
            "sqlx::postgres::PgPoolCopyExt::copy_in_raw",
            "std::fs::read",
        ];
        let macros = ["sqlx::query_unchecked", "sqlx::raw"];

        assert_eq!(found(&config(&methods, &macros)), []);
    }

    #[test]
    fn data_20_a_ban_of_a_sqlx_crate_that_reaches_nothing_fails() {
        let mut methods: Vec<&str> = TAKING_SQL[..11].to_vec();
        methods.extend([
            "sqlx::Pool::begin_without",
            "sqlx::nothing::here",
            "other_crate::anything",
        ]);
        let macros = ["sqlx::query_unchecked", "sqlx::raw", "sqlx::query_checked2"];

        assert_eq!(
            found(&config(&methods, &macros)),
            [
                ("sqlx::Pool::begin_without".to_owned(), 13),
                ("sqlx::nothing::here".to_owned(), 14),
                ("sqlx::query_checked2".to_owned(), 20),
            ]
        );
    }

    #[test]
    fn data_20_the_check_fails_when_sqlx_s_sql_types_are_missing() {
        let mut model = model();
        if let Some(root) = model
            .crates
            .first_mut()
            .and_then(|krate| krate.modules.first_mut())
        {
            root.items.retain(|item| !matches!(item, syn::Item::Use(_)));
        }

        let findings = audit(&model, "").findings;

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("doesn't export `SqlStr`"));
    }
}
