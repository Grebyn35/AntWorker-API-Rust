//! `sqlx-check`: the query cache in `.sqlx/` holds what `just prepare` would write (DATA-14), so a
//! query changed, or a migration that changed what a query reads, without `just prepare`, fails
//! here rather than building the image with the cache's stale description of a query. It runs
//! sqlx-cli's own check against a database of its own (`migrated`), which every copy of
//! `check-removable` builds from its own migrations.

use std::{ffi::OsString, process::Command};

use crate::{Finding, Outcome, migrated};

/// The directory of the query cache.
const CACHE: &str = ".sqlx/";

/// Compares the committed cache with what sqlx-cli writes from a migrated database.
pub fn check() -> Outcome {
    let checked = migrated::with_database("sqlx", |database| {
        let url = database
            .url("DATABASE_URL")
            .ok_or("DATABASE_URL isn't set: run the check through `just`, which reads .env")?;
        let output =
            Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")))
                .args([
                    "sqlx",
                    "prepare",
                    "--check",
                    "--workspace",
                    "--",
                    "--all-targets",
                    "--locked",
                ])
                .env("DATABASE_URL", url)
                .env_remove("SQLX_OFFLINE")
                .current_dir(&database.root)
                .output()
                .map_err(|error| {
                    format!(
                        "can't run sqlx-cli; `just setup` names the version to install: {error}"
                    )
                })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "the query cache differs from what the queries and the migrations give: run \
                 `just prepare` (DATA-14): {}",
                migrated::printed(&output)
            ))
        }
    });
    Outcome {
        report: Vec::new(),
        findings: checked
            .err()
            .map(|message| Finding::new(CACHE, 1, message))
            .into_iter()
            .collect(),
    }
}
