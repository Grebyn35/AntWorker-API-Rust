//! A database that the production `bootstrap` and `migrate` built: a database of its own on the
//! development Postgres, which `bootstrap` gives the roles and their grants, and
//! `migrate` the migrations, run from the API binary as an operator runs them. A check uses it,
//! then it is dropped. Its name holds the database the URLs name, the check's purpose, the
//! process's ID, so checks that run at once, as on the copies of the repository that checks make,
//! never share one, and the second at which it was created, by which a later run knows a
//! database that a stopped run left (OPS-21).

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::cargo;

/// The settings the API binary's `bootstrap` and `migrate` read for the database, which `just`
/// loads from `.env`.
const URLS: [&str; 3] = ["DATABASE_ADMIN_URL", "DATABASE_OWNER_URL", "DATABASE_URL"];

/// How many seconds after a database of a purpose was created a later run takes it for one that
/// its run left: longer than a check takes, or a run of the load test.
const LEFT_AFTER_SECONDS: u64 = 3_600;

/// What a database of a task's own is for, which its name holds.
#[derive(Clone, Copy)]
pub enum Purpose {
    /// `check-schema` reads its catalog.
    Schema,
    /// `sqlx-check` writes the query cache from it.
    Sqlx,
    /// `just new-module` writes the query cache from it.
    NewModule,
    /// `just load-test` runs its replica on it.
    Load,
}

impl Purpose {
    /// Every purpose.
    const ALL: [Self; 4] = [Self::Schema, Self::Sqlx, Self::NewModule, Self::Load];

    /// The purpose as a database's name holds it.
    fn name(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::Sqlx => "sqlx",
            Self::NewModule => "new_module",
            Self::Load => "load",
        }
    }
}

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

    /// Drops the database, with the connections it holds.
    pub fn dropped(&self) -> Result<(), String> {
        let sql = format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.name);
        psql(&self.root, &self.settings, &self.settings.database, &sql).map(drop)
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
    purpose: Purpose,
    check: impl FnOnce(&Database) -> Result<T, String>,
) -> Result<T, String> {
    let database = created(purpose)?;
    let result = build(&database).and_then(|()| check(&database));
    let dropped = database.dropped();
    let result = result?;
    dropped?;
    Ok(result)
}

/// Creates a database for `purpose` beside the one that the URLs name, on the development
/// Postgres, with nothing in it, which its task drops as it ends. First it drops each database
/// of a purpose whose name says that it was created more than an hour ago, which a run that was
/// stopped before it dropped its own left (OPS-21).
pub fn created(purpose: Purpose) -> Result<Database, String> {
    let settings = settings()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    #[expect(
        clippy::disallowed_methods,
        reason = "a task's database takes the time it was created in its name, which no code \
                  of the API reads"
    )]
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("the system's clock is before 1970: {error}"))?
        .as_secs();
    let on_base = |sql: &str| psql(&root, &settings, &settings.database, sql);
    let listed = on_base("SELECT datname FROM pg_database")?;
    for name in listed
        .lines()
        .filter(|name| left(name, &settings.database, now))
    {
        on_base(&format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))?;
    }
    let name = format!(
        "{}_{}_{}_{now}",
        settings.database,
        purpose.name(),
        std::process::id()
    );
    on_base(&format!("CREATE DATABASE \"{name}\""))?;
    Ok(Database {
        root,
        name,
        settings,
    })
}

/// Whether `name` is a database that a run created for a purpose beside `database` more than
/// an hour before `now`, seconds since 1970: `database`, a purpose, a process's ID and the second
/// of its creation, each after a `_`, the last two in digits alone.
fn left(name: &str, database: &str, now: u64) -> bool {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    Purpose::ALL.iter().any(|purpose| {
        name.strip_prefix(database)
            .and_then(|rest| rest.strip_prefix('_'))
            .and_then(|rest| rest.strip_prefix(purpose.name()))
            .and_then(|rest| rest.strip_prefix('_'))
            .and_then(|rest| rest.split_once('_'))
            .filter(|(process, _)| digits(process))
            .filter(|(_, created)| digits(created))
            .and_then(|(_, created)| created.parse::<u64>().ok())
            .is_some_and(|created| created.saturating_add(LEFT_AFTER_SECONDS) < now)
    })
}

/// Runs `bootstrap` and `migrate` on `database`.
fn build(database: &Database) -> Result<(), String> {
    for subcommand in ["bootstrap", "migrate"] {
        let mut command = cargo::cargo();
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
    fn ops_21_a_database_of_a_purpose_counts_as_left_an_hour_after_its_creation() {
        let now = 1_800_000_000;
        for name in [
            "api_schema_4711_1799990000",
            "api_sqlx_1_0",
            "api_new_module_4711_1799996399",
            "api_load_4711_1799990000",
        ] {
            assert!(left(name, "api", now), "{name}");
        }
        for name in [
            // Created within the hour, at this second, and after it.
            "api_schema_4711_1799996400",
            "api_schema_4711_1800000000",
            "api_schema_4711_1800000001",
            // No purpose's, another database's, and names that hold more or less.
            "api",
            "api_backup_4711_1799990000",
            "other_schema_4711_1799990000",
            "apis_schema_4711_1799990000",
            "api_schema_4711",
            "api_schema_4711_",
            "api_schema__1799990000",
            "api_schema_4711_1799990000_1",
            "api_schema_x4711_1799990000",
            "api_schema_4711_17999900.0",
            "api_schema_new_module_4711_1799990000",
        ] {
            assert!(!left(name, "api", now), "{name}");
        }
    }

    #[test]
    fn ops_21_a_task_s_database_replaces_the_one_each_url_names() {
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
