//! `new-module`, which writes a new module that holds one workspace resource, and
//! `check-new-module`, which writes one on a copy of the repository and runs `just checks` there
//! (ARCH-03, docs/guides/adding-a-module.md).
//!
//! The templates in `crates/xtask/templates/new-module/` are the module `sample_items`, whose
//! resource is `sample_item`, written as the code and documents of any module are: `files/` holds
//! the files the module owns, at their paths, each ending in `.tmpl`, and `insert/` what it adds to
//! the files every module shares, which `edits` places. Writing a module replaces each form of the
//! template's names with the same form of the module's (`names`).
//!
//! The module's queries need the query cache before anything that holds them builds (DATA-14), and
//! the cache needs a database that the module's migration built, so the task writes the migration
//! first, builds a database of its own from it (`migrated`), with the code that has no query of the
//! module yet, then writes the rest, rewrites the cache from that database and drops it. It never
//! touches the development database. A step that fails undoes what the task wrote.

mod edits;
mod names;

use std::{
    io,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use edits::{Edit, Inserts};
use names::Names;

use crate::{
    copy::{self, Copy},
    migrated,
    repo::{self, Repo},
};

/// The templates' directory, from the repository root.
const TEMPLATES: &str = "crates/xtask/templates/new-module";

/// The suffix of every template's file name.
const SUFFIX: &str = ".tmpl";

/// What a template's migration holds in place of its timestamp.
const TIMESTAMP: &str = "TIMESTAMP";

/// The migrations' directory.
const MIGRATIONS: &str = "migrations/";

/// The query cache's directory (DATA-14).
const CACHE: &str = ".sqlx/";

/// The module that `check-new-module` writes: its two words take every form apart, and its
/// resource takes "an".
const CHECKED: (&str, &str) = ("order_items", "order_item");

/// Writes the module `module`, whose resource is `resource`, or the singular of `module`.
pub fn run(module: &str, resource: Option<String>) -> ExitCode {
    let names = match Names::new(module, resource.as_deref()) {
        Ok(names) => names,
        Err(error) => return fail(&error),
    };
    match write(&names) {
        Ok(written) => {
            report(&format!(
                "new-module: wrote `{}`, whose resource is `{}`, in {} files, and registered it in \
                 {} more:",
                names.module,
                names.resource,
                written.created.len(),
                written.edited.len()
            ));
            for path in written.created.iter().chain(&written.edited) {
                report(&format!("  {path}"));
            }
            report(
                "Next: rewrite its spec's Purpose and behaviors for what the module is for, then \
                 its code and tests to match, as docs/guides/adding-a-module.md says. `just dev` \
                 and `just migrate` apply its migration to the development database.",
            );
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error),
    }
}

/// Runs `check-new-module` (ARCH-03): on a fresh copy of the repository, writes a module with
/// `just new-module`, then runs `just checks` there.
pub fn check() -> ExitCode {
    let (module, resource) = CHECKED;
    let checked = Copy::fresh("new-module", module).and_then(|copy| {
        copy.just(&["new-module", module, resource])?;
        copy.just(&["checks"])
    });
    match checked {
        Ok(()) => {
            report(&format!("check-new-module: `{module}` passed the checks"));
            ExitCode::SUCCESS
        }
        Err(error) => fail(&format!("check-new-module: `{module}` failed: {error}")),
    }
}

/// What writing a module created and changed.
struct Written {
    /// The files it created.
    created: Vec<String>,
    /// The shared files it changed.
    edited: Vec<String>,
}

/// A file as it stood before the task, so a failure puts it back: `None` for one it created.
type Original = (String, Option<String>);

/// Writes the module `names`, or undoes what it wrote and says why it couldn't.
fn write(names: &Names) -> Result<Written, String> {
    let repo = Repo::load().map_err(|error| format!("can't read the repository: {error}"))?;
    refuse_taken(&repo, names)?;
    let files = files(&repo, names)?;
    let inserts = inserts(&repo, names)?;
    let edits = edits::edits(names, &inserts, |path| {
        repo.file(path).map(|file| file.text.clone())
    })
    .map_err(|missing| {
        format!(
            "these shared files don't hold the places the module registers itself in, so \
                 nothing was written; add each by hand after writing the module from a copy whose \
                 files do, or restore them:\n  {}",
            missing.join("\n  ")
        )
    })?;
    let cache = snapshot(&repo, CACHE);
    let mut originals: Vec<Original> = Vec::new();
    let written = apply(&files, &edits, &mut originals);
    match written {
        Ok(()) => Ok(Written {
            created: files.iter().map(|(path, _)| path.clone()).collect(),
            edited: edits.iter().map(|edit| edit.path.to_owned()).collect(),
        }),
        Err(error) => {
            let undone = undo(&originals, &cache);
            Err(match undone {
                Ok(()) => format!("{error}\nThe task undid what it wrote."),
                Err(undo) => format!("{error}\nUndoing what it wrote failed too: {undo}"),
            })
        }
    }
}

/// Fails when the repository already has a module, a spec, a test file or a migration of
/// `names`' module.
fn refuse_taken(repo: &Repo, names: &Names) -> Result<(), String> {
    let module = &names.module;
    let taken = [
        format!("crates/app/src/modules/{module}/"),
        format!("docs/spec/{module}.md"),
        format!("crates/app/tests/integration/{module}.rs"),
    ];
    let migration = format!("_{module}_");
    let found = repo.paths().find(|path| {
        taken.iter().any(|taken| path.starts_with(taken.as_str()))
            || path
                .strip_prefix(MIGRATIONS)
                .is_some_and(|name| name.contains(&migration))
    });
    match found {
        Some(path) => Err(format!(
            "the repository has `{path}` already, so it has a module `{module}`, or had one"
        )),
        None => Ok(()),
    }
}

/// Each file the module owns, at its path, with its text: the templates of `files/` with
/// `names` in place of the template's, the migration's timestamp after every other migration's.
fn files(repo: &Repo, names: &Names) -> Result<Vec<(String, String)>, String> {
    let prefix = format!("{TEMPLATES}/files/");
    let timestamp = timestamp(repo)?;
    let files: Vec<(String, String)> = repo
        .files()
        .filter_map(|file| {
            let path = file.path.strip_prefix(&prefix)?.strip_suffix(SUFFIX)?;
            let path = names.apply(path).replace(TIMESTAMP, &timestamp);
            Some((path, names.apply(&file.text)))
        })
        .collect();
    if files.is_empty() {
        return Err(format!("there is no template under {prefix}"));
    }
    leftovers(
        names,
        files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str())),
    )?;
    Ok(files)
}

/// The templates of `insert/`, with `names` in place of the template's.
fn inserts(repo: &Repo, names: &Names) -> Result<Inserts, String> {
    let read = |name: &str| {
        let path = format!("{TEMPLATES}/insert/{name}{SUFFIX}");
        repo.file(&path)
            .map(|file| names.apply(&file.text))
            .ok_or_else(|| format!("there is no template {path}"))
    };
    let inserts = Inserts {
        permissions: read("permissions.rs")?,
        matrix: read("matrix.rs")?,
        access_exports: read("access-exports.rs")?,
        test_08_row: read("test-08-row.rs")?,
        test_08_fixtures: read("test-08-fixtures.rs")?,
        architecture_owns: read("architecture-owns.md")?,
        architecture_graph: read("architecture-graph.md")?,
        spec_index: read("spec-index.md")?,
        access_matrix: read("access-matrix.md")?,
    };
    leftovers(
        names,
        [
            ("permissions.rs", inserts.permissions.as_str()),
            ("matrix.rs", &inserts.matrix),
            ("access-exports.rs", &inserts.access_exports),
            ("test-08-row.rs", &inserts.test_08_row),
            ("test-08-fixtures.rs", &inserts.test_08_fixtures),
            ("architecture-owns.md", &inserts.architecture_owns),
            ("architecture-graph.md", &inserts.architecture_graph),
            ("spec-index.md", &inserts.spec_index),
            ("access-matrix.md", &inserts.access_matrix),
        ],
    )?;
    Ok(inserts)
}

/// Fails on a form of the template's names left in what the module's files hold, unless the
/// module is the template's: a form that `names` doesn't replace.
fn leftovers<'a>(
    names: &Names,
    texts: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<(), String> {
    if *names == Names::template() || names.module.contains("sample") {
        return Ok(());
    }
    for (path, text) in texts {
        let lower = text.to_ascii_lowercase();
        if lower.contains("sample") {
            return Err(format!(
                "the template of {path} holds a form of `sample_items` that the task doesn't \
                 replace: write it in one of the forms that crates/xtask/src/new_module/names.rs \
                 lists"
            ));
        }
    }
    Ok(())
}

/// The migration's timestamp: the time now, in UTC, as `YYYYMMDDhhmmss`, or one second after the
/// newest migration's, if that comes later, so the module's migration runs after every other.
fn timestamp(repo: &Repo) -> Result<String, String> {
    #[expect(
        clippy::disallowed_methods,
        reason = "a migration's name takes the time it was written, which no code of the API reads"
    )]
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("the system's clock is before 1970: {error}"))?
        .as_secs();
    let now: u64 = civil(now)
        .parse()
        .map_err(|_| "the time isn't a number".to_owned())?;
    let newest = repo
        .paths()
        .filter_map(|path| path.strip_prefix(MIGRATIONS))
        .filter_map(|name| name.split('_').next()?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    Ok(now.max(newest + 1).to_string())
}

/// The moment `seconds` after the Unix epoch, in UTC, as `YYYYMMDDhhmmss`, by the proleptic
/// Gregorian calendar.
fn civil(seconds: u64) -> String {
    let days = seconds / 86_400;
    let of_day = seconds % 86_400;
    // Howard Hinnant's algorithm from days to the civil calendar, whose years start in March, so
    // a leap day ends one.
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60
    )
}

/// The files under `directory`, with their text, so a failure puts them back.
fn snapshot(repo: &Repo, directory: &str) -> Vec<(String, String)> {
    repo.files()
        .filter(|file| file.path.starts_with(directory))
        .map(|file| (file.path.clone(), file.text.clone()))
        .collect()
}

/// Writes the migration, builds a database from it, then writes the module's other files and the
/// edits, rewrites the query cache from that database, writes `openapi.json` again and formats the
/// code. Each file it writes is recorded in `originals` first.
fn apply(
    files: &[(String, String)],
    edits: &[Edit],
    originals: &mut Vec<Original>,
) -> Result<(), String> {
    let (migrations, rest): (Vec<_>, Vec<_>) = files
        .iter()
        .partition(|(path, _)| path.starts_with(MIGRATIONS));
    for (path, text) in migrations {
        create(path, text, originals)?;
    }
    migrated::with_database("new_module", |database| {
        for (path, text) in &rest {
            create(path, text, originals)?;
        }
        for edit in edits {
            let original = std::str::from_utf8(
                &repo::read(&copy::root().join(edit.path))
                    .map_err(|error| format!("can't read {}: {error}", edit.path))?,
            )
            .map(str::to_owned)
            .map_err(|error| format!("{} isn't text: {error}", edit.path))?;
            originals.push((edit.path.to_owned(), Some(original)));
            repo::write(edit.path, &edit.text)
                .map_err(|error| format!("can't write {}: {error}", edit.path))?;
        }
        let url = database
            .url("DATABASE_URL")
            .ok_or("DATABASE_URL isn't set: run the task through `just`, which reads .env")?;
        copy::run(
            &database.root,
            "cargo",
            &[
                "sqlx",
                "prepare",
                "--workspace",
                "--",
                "--all-targets",
                "--locked",
            ],
            &[("DATABASE_URL", url.as_str()), ("SQLX_OFFLINE", "false")],
        )
    })?;
    let root = copy::root();
    copy::run(
        &root,
        "cargo",
        &[
            "run",
            "--locked",
            "--quiet",
            "--package",
            "app",
            "--bin",
            "antworker-api",
            "--",
            "openapi",
            "openapi.json",
        ],
        &[("SQLX_OFFLINE", "true")],
    )?;
    copy::run(&root, "cargo", &["fmt", "--all"], &[])
}

/// Creates the file at `path` with `text`, recording that it didn't exist.
fn create(path: &str, text: &str, originals: &mut Vec<Original>) -> Result<(), String> {
    originals.push((path.to_owned(), None));
    repo::write(path, text).map_err(|error| format!("can't write {path}: {error}"))
}

/// Puts back each file of `originals` as it stood, deleting those the task created, and the query
/// cache as `cache` holds it, then `openapi.json` as git has it.
fn undo(originals: &[Original], cache: &[(String, String)]) -> io::Result<()> {
    for (path, original) in originals.iter().rev() {
        match original {
            Some(text) => repo::write(path, text)?,
            None => match repo::delete(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            },
        }
    }
    repo::empty_directory(CACHE)?;
    for (path, text) in cache {
        repo::write(path, text)?;
    }
    copy::run(
        &copy::root(),
        "git",
        &["checkout", "--", "openapi.json"],
        &[],
    )
    .map_err(io::Error::other)
}

#[expect(clippy::print_stdout, reason = "the report is the task's output")]
fn report(message: &str) {
    println!("{message}");
}

#[expect(clippy::print_stderr, reason = "the error is the task's output")]
fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_03_a_migration_s_timestamp_is_the_utc_time_it_was_written() {
        assert_eq!(civil(0), "19700101000000");
        assert_eq!(civil(951_782_400), "20000229000000");
        assert_eq!(civil(1_793_532_345), "20261101112545");
    }

    #[test]
    fn arch_03_the_new_migration_comes_after_every_other() {
        let repo = Repo::of(&[("migrations/99990101000000_x_x.sql", "")]);
        assert_eq!(timestamp(&repo).unwrap(), "99990101000001");
    }

    #[test]
    fn arch_03_a_module_that_exists_or_existed_is_refused() {
        let names = Names::new("order_items", None).unwrap();
        let spec = Repo::of(&[("docs/spec/order_items.md", "")]);
        let migration = Repo::of(&[("migrations/20260101000000_order_items_order_items.sql", "")]);
        let other = Repo::of(&[("migrations/20260101000000_orders_orders.sql", "")]);

        assert!(refuse_taken(&spec, &names).is_err());
        assert!(refuse_taken(&migration, &names).is_err());
        assert!(refuse_taken(&other, &names).is_ok());
    }

    #[test]
    fn arch_03_every_template_name_becomes_the_module_s() {
        let repo = Repo::load().unwrap();
        let names = Names::new(CHECKED.0, Some(CHECKED.1)).unwrap();

        let files = files(&repo, &names).unwrap();
        inserts(&repo, &names).unwrap();

        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert!(paths.contains(&"crates/app/src/modules/order_items/mod.rs"));
        assert!(paths.contains(&"docs/spec/order_items.md"));
        assert!(paths.iter().any(|path| {
            path.starts_with("migrations/") && path.ends_with("_order_items_order_items.sql")
        }));
    }
}
