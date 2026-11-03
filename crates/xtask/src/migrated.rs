//! A database that the production `bootstrap` and `migrate` built: a database of its own on the
//! Postgres that `just services` runs, which `bootstrap` gives the roles and their grants, and
//! `migrate` the migrations, run from the API binary as an operator runs them. A check uses it,
//! then it is dropped. Its name holds the database the URLs name, the check's purpose and the
//! process's ID, so checks that run at once, as in each copy that `check-removable` makes, never
//! share one.

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// The settings the API binary's `bootstrap` and `migrate` read for the database, which `just`
/// loads from `.env`.
const URLS: [&str; 3] = ["DATABASE_ADMIN_URL", "DATABASE_OWNER_URL", "DATABASE_URL"];

/// A migrated database of a check's own.
pub struct Database {
    /// The repository's root.
    pub root: PathBuf,
    /// Its name.
    name: String,
    settings: Settings,
}

impl Database {
    /// The URL of `setting`, one of the three, with this database in place of the one it names.
    pub fn url(&self, setting: &str) -> Option<String> {
        self.settings
            .urls
            .iter()
            .find(|(name, _)| *name == setting)
            .map(|(_, url)| with_database_name(url, &self.name))
    }

    /// Runs `sql` on this database with psql, as the administrator, and returns what it printed.
    pub fn psql(&self, sql: &str) -> Result<String, String> {
        psql(&self.root, &self.settings, &self.name, sql)
    }
}

/// The database's settings, from the environment.
struct Settings {
    /// The three URLs, by setting.
    urls: Vec<(&'static str, String)>,
    /// The database the URLs name, from which a check's database takes its name.
    database: String,
    /// The administrator's user, as which psql connects in the container.
    admin: String,
}

/// Builds a database for `purpose` with the production `bootstrap` and `migrate`, runs `check` on
/// it, and drops it, whatever `check` found.
pub fn with_database<T>(
    purpose: &str,
    check: impl FnOnce(&Database) -> Result<T, String>,
) -> Result<T, String> {
    let settings = settings()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let name = format!("{}_{purpose}_{}", settings.database, std::process::id());
    psql(
        &root,
        &settings,
        &settings.database,
        &format!("CREATE DATABASE \"{name}\""),
    )?;
    let database = Database {
        root,
        name,
        settings,
    };
    let result = build(&database).and_then(|()| check(&database));
    let dropped = psql(
        &database.root,
        &database.settings,
        &database.settings.database,
        &format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", database.name),
    );
    let result = result?;
    dropped?;
    Ok(result)
}

/// Runs `bootstrap` and `migrate` on `database`.
fn build(database: &Database) -> Result<(), String> {
    for subcommand in ["bootstrap", "migrate"] {
        let mut command =
            Command::new(std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo")));
        command
            .args([
                "run",
                "--locked",
                "--quiet",
                "--package",
                "app",
                "--bin",
                "antworker-api",
                "--",
            ])
            .arg(subcommand)
            .current_dir(&database.root);
        for (setting, url) in &database.settings.urls {
            command.env(setting, with_database_name(url, &database.name));
        }
        let output = command
            .output()
            .map_err(|error| format!("can't run `{subcommand}`: {error}"))?;
        if !output.status.success() {
            return Err(format!("`{subcommand}` failed: {}", printed(&output)));
        }
    }
    Ok(())
}

/// Runs `sql` with psql in the container of Compose's `postgres`, on `database`, as the
/// administrator, and returns what it printed.
fn psql(root: &Path, settings: &Settings, database: &str, sql: &str) -> Result<String, String> {
    let output = Command::new("docker")
        .args(["compose", "exec", "--no-TTY", "postgres", "psql"])
        .args(["--no-psqlrc", "--no-align", "--tuples-only", "--quiet"])
        .args([
            "--set",
            "ON_ERROR_STOP=1",
            "--username",
            &settings.admin,
            "--dbname",
            database,
        ])
        .args(["--command", sql])
        .current_dir(root)
        .output()
        .map_err(|error| format!("can't run psql through Docker Compose: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "psql failed; `just services` starts Postgres: {}",
            printed(&output)
        ))
    }
}

/// What a process printed, for its failure.
pub fn printed(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stderr).trim(),
        String::from_utf8_lossy(&output.stdout).trim()
    )
}

/// The three URLs from the environment, the database they name and the administrator.
fn settings() -> Result<Settings, String> {
    let mut urls = Vec::new();
    for setting in URLS {
        let url = std::env::var(setting).map_err(|_| {
            format!("{setting} isn't set: run the check through `just`, which reads .env")
        })?;
        urls.push((setting, url));
    }
    let admin = urls
        .first()
        .map(|(_, url)| url.as_str())
        .unwrap_or_default();
    let database = admin
        .split('?')
        .next()
        .and_then(|url| url.rsplit_once('/'))
        .map(|(_, database)| database.to_owned())
        .filter(|database| !database.is_empty())
        .ok_or("DATABASE_ADMIN_URL names no database")?;
    let user = admin
        .split_once("://")
        .and_then(|(_, rest)| rest.split(['@', ':']).next())
        .map(str::to_owned)
        .filter(|user| !user.is_empty())
        .ok_or("DATABASE_ADMIN_URL names no user")?;
    Ok(Settings {
        urls,
        database,
        admin: user,
    })
}

/// `url` with its database replaced by `database`.
fn with_database_name(url: &str, database: &str) -> String {
    let (base, query) = url
        .split_once('?')
        .map_or((url, None), |(b, q)| (b, Some(q)));
    let base = base
        .rsplit_once('/')
        .map_or(base, |(authority, _)| authority);
    match query {
        Some(query) => format!("{base}/{database}?{query}"),
        None => format!("{base}/{database}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_19_a_check_s_database_replaces_the_one_each_url_names() {
        assert_eq!(
            with_database_name("postgres://app:pw@127.0.0.1:5432/api", "api_schema_7"),
            "postgres://app:pw@127.0.0.1:5432/api_schema_7"
        );
        assert_eq!(
            with_database_name("postgres://owner@db/api?sslmode=require", "api_schema_7"),
            "postgres://owner@db/api_schema_7?sslmode=require"
        );
    }
}
