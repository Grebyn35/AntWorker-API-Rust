//! `check-migrations`: the migrations keep their history (DATA-09).
//! - Each file of `migrations/` is named `<timestamp>_<module>_<description>.sql`: fourteen
//!   digits, a module of `app` or `platform`, then a description in `snake_case`.
//! - The newest release holds its migrations as they are: none of the files under `migrations/`
//!   of the newest tag `v*` is changed or deleted, since `migrate` and `serve` refuse a database
//!   whose applied migration differs from its file (PLATFORM-029, PLATFORM-030). A repository
//!   without a release, such as a fork before its first, has nothing to keep.
//!
//! It reads the newest release from git's tags, so a checkout needs them: CI's fetches them.

use std::{path::Path, process::Command};

use crate::{
    Finding, Outcome,
    repo::{self, Repo},
};

/// The directory of the migrations.
const MIGRATIONS: &str = "migrations/";

/// The directory of `app`'s modules, whose names a migration's name may hold.
const MODULES: &str = "crates/app/src/modules/";

/// The tags of releases (OPS-14).
const RELEASE_TAGS: &str = "v*";

/// A release, with the migrations it holds: each one's path and bytes.
pub struct Release {
    tag: String,
    migrations: Vec<(String, Vec<u8>)>,
}

/// Checks the repository's migrations against their names and the newest release.
pub fn check(repo: &Repo) -> Outcome {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    match newest_release(&root) {
        Ok(release) => check_with(repo, release.as_ref(), |path| repo::read(&root.join(path))),
        Err(error) => Outcome {
            report: Vec::new(),
            findings: vec![Finding::new(MIGRATIONS, 1, error)],
        },
    }
}

/// Checks `repo`'s migrations against their names, and against `release`'s, reading the bytes of
/// a migration with `read`.
fn check_with(
    repo: &Repo,
    release: Option<&Release>,
    read: impl Fn(&str) -> std::io::Result<Vec<u8>>,
) -> Outcome {
    let mut modules: Vec<&str> = repo
        .paths()
        .filter_map(|path| path.strip_prefix(MODULES))
        .filter_map(|rest| rest.split_once('/').map(|(module, _)| module))
        .collect();
    modules.push("platform");
    let mut findings = Vec::new();
    for path in repo.paths().filter(|path| path.starts_with(MIGRATIONS)) {
        if !named(path, &modules) {
            findings.push(Finding::new(
                path,
                1,
                "a migration is named `<timestamp>_<module>_<description>.sql`: fourteen digits, \
                 a module of `app` or `platform`, then a description in snake_case (DATA-09)",
            ));
        }
    }
    let report = match release {
        None => "check-migrations: no release holds a migration yet.".to_owned(),
        Some(release) => {
            for (path, bytes) in &release.migrations {
                let message = match read(path) {
                    Ok(current) if current == *bytes => continue,
                    Ok(_) => format!(
                        "{} holds this migration as it was: a migration in a release is never \
                         edited, so a new migration makes the change (DATA-09)",
                        release.tag
                    ),
                    Err(_) => format!(
                        "{} holds this migration, which is gone: a migration in a release is \
                         never deleted (DATA-09)",
                        release.tag
                    ),
                };
                findings.push(Finding::new(path, 1, message));
            }
            format!(
                "check-migrations: {} migrations of {} unchanged.",
                release.migrations.len(),
                release.tag
            )
        }
    };
    Outcome {
        report: vec![report],
        findings,
    }
}

/// Whether `path` names a migration as DATA-09 asks, with one of `modules`.
fn named(path: &str, modules: &[&str]) -> bool {
    let Some(name) = path
        .strip_prefix(MIGRATIONS)
        .and_then(|name| name.strip_suffix(".sql"))
    else {
        return false;
    };
    let Some((timestamp, rest)) = name.split_once('_') else {
        return false;
    };
    timestamp.len() == 14
        && timestamp.chars().all(|c| c.is_ascii_digit())
        && modules.iter().any(|module| {
            rest.strip_prefix(module)
                .and_then(|rest| rest.strip_prefix('_'))
                .is_some_and(|description| {
                    !description.is_empty()
                        && description.split('_').all(|word| {
                            !word.is_empty()
                                && word
                                    .chars()
                                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                        })
                })
        })
}

/// The newest release of the repository at `root`, by version, with its migrations, or `None`
/// when no tag names one.
fn newest_release(root: &Path) -> Result<Option<Release>, String> {
    let tags = git(
        root,
        &["tag", "--list", RELEASE_TAGS, "--sort=-version:refname"],
    )?;
    let Some(tag) = String::from_utf8_lossy(&tags)
        .lines()
        .next()
        .map(str::to_owned)
    else {
        return Ok(None);
    };
    let listing = git(
        root,
        &["ls-tree", "-r", "-z", "--name-only", &tag, "--", MIGRATIONS],
    )?;
    let mut migrations = Vec::new();
    for path in String::from_utf8_lossy(&listing)
        .split('\0')
        .filter(|path| !path.is_empty())
    {
        let bytes = git(root, &["show", &format!("{tag}:{path}")])?;
        migrations.push((path.to_owned(), bytes));
    }
    Ok(Some(Release { tag, migrations }))
}

/// What `git args` writes, run at `root`.
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(outcome: &Outcome) -> Vec<String> {
        outcome
            .findings
            .iter()
            .map(|finding| format!("{}: {}", finding.path, finding.message))
            .collect()
    }

    #[test]
    fn data_09_a_migration_names_its_module() {
        let repo = Repo::of(&[
            ("crates/app/src/modules/client_telemetry/mod.rs", ""),
            ("migrations/20260101000000_platform_jobs.sql", ""),
            ("migrations/20260101000100_client_telemetry_reports.sql", ""),
            ("migrations/20260101000200_billing_plans.sql", ""),
            ("migrations/2026_platform_x.sql", ""),
            ("migrations/20260101000300_platform_Jobs.sql", ""),
        ]);

        let outcome = check_with(&repo, None, |_| Ok(Vec::new()));

        let paths: Vec<&str> = outcome
            .findings
            .iter()
            .map(|finding| finding.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "migrations/20260101000200_billing_plans.sql",
                "migrations/2026_platform_x.sql",
                "migrations/20260101000300_platform_Jobs.sql",
            ]
        );
        assert_eq!(
            outcome.report,
            ["check-migrations: no release holds a migration yet."]
        );
    }

    #[test]
    fn data_09_a_migration_in_a_release_is_neither_edited_nor_deleted() {
        let repo = Repo::of(&[
            (
                "migrations/20260101000000_platform_jobs.sql",
                "CREATE TABLE jobs ();\n",
            ),
            (
                "migrations/20260101000100_platform_events.sql",
                "CREATE TABLE events (id uuid);\n",
            ),
        ]);
        let release = Release {
            tag: "v1.0.0".to_owned(),
            migrations: vec![
                (
                    "migrations/20260101000000_platform_jobs.sql".to_owned(),
                    b"CREATE TABLE jobs ();\n".to_vec(),
                ),
                (
                    "migrations/20260101000100_platform_events.sql".to_owned(),
                    b"CREATE TABLE events ();\n".to_vec(),
                ),
                (
                    "migrations/20260101000200_platform_gone.sql".to_owned(),
                    b"SELECT 1;\n".to_vec(),
                ),
            ],
        };

        let outcome = check_with(&repo, Some(&release), |path| {
            repo.file(path)
                .map(|file| file.text.clone().into_bytes())
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
        });

        assert_eq!(
            messages(&outcome),
            [
                "migrations/20260101000100_platform_events.sql: v1.0.0 holds this migration as it \
                 was: a migration in a release is never edited, so a new migration makes the \
                 change (DATA-09)",
                "migrations/20260101000200_platform_gone.sql: v1.0.0 holds this migration, which \
                 is gone: a migration in a release is never deleted (DATA-09)",
            ]
        );
    }
}
