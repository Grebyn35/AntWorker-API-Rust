//! The copies of the repository that a check changes as a fork does, then runs `just checks` on,
//! such as the one that `check-new-module` adds two modules to, and the one in which
//! `check-rename` renames the product. Each copy is in `target/<check>/<name>/repo/`, with its
//! build in `target/<check>/<name>/target/`, so no copy's `cargo run` finds a binary that another
//! built.
//! A copy builds without incremental compilation, and before a check makes one, it removes the
//! copies made longest ago while they hold more than [`BOUND`] together (OPS-21).

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use crate::cargo;

/// The bytes that the copies hold together, at most, when a check makes one: 60 gibibytes
/// (OPS-21).
const BOUND: u64 = 60 * 1024 * 1024 * 1024;

/// The repository's root.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs `program` with `args` in `directory`, with `environment` added and without the variables
/// in which Cargo described this program's package, and fails unless it succeeds. It writes to
/// the task's own output.
pub fn run(
    directory: &Path,
    program: &str,
    args: &[&str],
    environment: &[(&str, &str)],
) -> Result<(), String> {
    let status = cargo::command(program)
        .args(args)
        .envs(environment.iter().copied())
        .current_dir(directory)
        .status()
        .map_err(|error| format!("can't run {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{program} {}` failed: {status}", args.join(" ")))
    }
}

/// A fresh copy of the repository, which shares the repository's `.env` and its Compose project,
/// so its tests use the same tests' Postgres and Mailpit: there, a test run's template database
/// is named after its migrations, and each test's database after its process.
pub struct Copy {
    /// The copy's root.
    root: PathBuf,
    /// Its build directory.
    target: String,
    /// The repository's Compose project.
    project: String,
}

impl Copy {
    /// A fresh copy for `check`, named `name`, made once the copies hold no more than [`BOUND`]
    /// together.
    pub fn fresh(check: Check, name: &str) -> Result<Self, String> {
        Self::of(&root(), check, name, BOUND)
    }

    /// A fresh copy of the repository at `root`, under its `target/`, made once the copies there
    /// hold no more than `bound` bytes together.
    fn of(root: &Path, check: Check, name: &str, bound: u64) -> Result<Self, String> {
        let target = root.join("target");
        let directory = target.join(check.directory()).join(name);
        let removed = remove_beyond(&target, &directory, bound)
            .map_err(|error| format!("can't bound what the copies hold: {error}"))?;
        for removed in removed {
            report(&format!(
                "removed {}, a copy and its build: the checks' copies held more than {} GiB together",
                removed.strip_prefix(root).unwrap_or(&removed).display(),
                bound >> 30
            ));
        }
        let copy = directory.join("repo");
        fresh_copy(root, &copy).map_err(|error| format!("can't copy the repository: {error}"))?;
        Ok(Self {
            root: copy,
            target: directory.join("target").to_string_lossy().into_owned(),
            project: project(),
        })
    }

    /// Runs `just` with `args` on the copy, and fails unless it succeeds. The copy builds
    /// without incremental compilation, whose caches would roughly double what its build
    /// directory holds: a copy's files are new each time, so every run compiles the workspace's
    /// crates again.
    pub fn just(&self, args: &[&str]) -> Result<(), String> {
        run(
            &self.root,
            "just",
            args,
            &[
                ("CARGO_TARGET_DIR", self.target.as_str()),
                ("CARGO_INCREMENTAL", "0"),
                ("COMPOSE_PROJECT_NAME", self.project.as_str()),
            ],
        )
    }
}

/// A check that changes copies of the repository.
#[derive(Clone, Copy)]
pub enum Check {
    /// `check-new-module`.
    NewModule,
    /// `check-rename`.
    Rename,
    // part: removal
    /// `check-removable`, which makes a copy for each of its runs.
    Removable,
    // end part: removal
}

impl Check {
    /// Every check, since their copies count together against [`BOUND`].
    const ALL: &[Self] = &[
        Self::NewModule,
        Self::Rename,
        // part: removal
        Self::Removable,
        // end part: removal
    ];

    /// The directory under `target/` that holds the check's copies.
    const fn directory(self) -> &'static str {
        match self {
            // part: removal
            Self::Removable => "removable",
            // end part: removal
            Self::NewModule => "new-module",
            Self::Rename => "rename",
        }
    }
}

/// A copy that a check left under `target/`.
struct Held {
    /// The copy's directory, which holds its files and its build.
    directory: PathBuf,
    /// The bytes of the files under it, a file counted once for each of its names.
    bytes: u64,
    /// When a check last made the copy: when the directory last changed, as it does when a check
    /// replaces the copy's files.
    made: SystemTime,
}

/// Removes copies under `target`, the repository's `target/`, until the rest hold at most
/// `bound` bytes together, and returns the ones it removed. `making` is the directory of the
/// copy that a check is about to make.
#[expect(
    clippy::disallowed_methods,
    reason = "the checks run as a synchronous program, with no async runtime to stall"
)]
fn remove_beyond(target: &Path, making: &Path, bound: u64) -> io::Result<Vec<PathBuf>> {
    let removed = beyond(held(target)?, making, bound);
    for directory in &removed {
        std::fs::remove_dir_all(directory)?;
    }
    Ok(removed)
}

/// The copies to remove so that the rest hold at most `bound` bytes together: the ones made
/// longest ago first, and `making`, whose build its check is about to use, last.
fn beyond(mut held: Vec<Held>, making: &Path, bound: u64) -> Vec<PathBuf> {
    held.sort_by(|a, b| {
        (a.directory == making, a.made, &a.directory).cmp(&(
            b.directory == making,
            b.made,
            &b.directory,
        ))
    });
    let mut bytes: u64 = held.iter().map(|copy| copy.bytes).sum();
    let mut removed = Vec::new();
    for copy in held {
        if bytes <= bound {
            break;
        }
        bytes -= copy.bytes;
        removed.push(copy.directory);
    }
    removed
}

/// The copies under `target`, the repository's `target/`: each directory of each check's.
#[expect(
    clippy::disallowed_methods,
    reason = "the checks run as a synchronous program, with no async runtime to stall"
)]
fn held(target: &Path) -> io::Result<Vec<Held>> {
    let mut held = Vec::new();
    for &check in Check::ALL {
        let entries = match std::fs::read_dir(target.join(check.directory())) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let directory = entry.path();
            held.push(Held {
                bytes: bytes(&directory)?,
                made: entry.metadata()?.modified()?,
                directory,
            });
        }
    }
    Ok(held)
}

/// The bytes of the files under `directory`, without following a link.
#[expect(
    clippy::disallowed_methods,
    reason = "the checks run as a synchronous program, with no async runtime to stall"
)]
fn bytes(directory: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            total += bytes(&entry.path())?;
        } else if kind.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

#[expect(clippy::print_stdout, reason = "the report is the task's output")]
fn report(message: &str) {
    println!("{message}");
}

/// The name of the repository's Compose project: `COMPOSE_PROJECT_NAME` if it is set, or the
/// name of the root's directory, as Compose makes a project's name of it.
fn project() -> String {
    if let Some(name) = std::env::var_os("COMPOSE_PROJECT_NAME") {
        return name.to_string_lossy().into_owned();
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
        .collect()
}

/// Makes `copy` a fresh copy of the files git tracks, or would track once added, in `root`, and
/// of `.env`, as a git repository of its own with one commit, so the copy's tasks that ask git
/// for its files find the copy's.
#[expect(
    clippy::disallowed_methods,
    reason = "the checks run as a synchronous program, with no async runtime to stall"
)]
fn fresh_copy(root: &Path, copy: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(copy) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    std::fs::create_dir_all(copy)?;
    let listing = Command::new("git")
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()?;
    if !listing.status.success() {
        return Err(io::Error::other("git ls-files failed"));
    }
    let listing = String::from_utf8_lossy(&listing.stdout);
    let paths = listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .chain([".env"]);
    for path in paths {
        let from = root.join(path);
        // git still lists a file deleted from the working tree until the deletion is staged, and
        // a machine may have no `.env`.
        if !from.is_file() {
            continue;
        }
        let to = copy.join(path);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to)?;
    }
    for args in [
        &["init", "--quiet"][..],
        &["add", "--all"],
        // Without git's maintenance after the commit, which a newer git runs in the background:
        // it would write and remove its files under the copy's `.git` while the next check
        // counts or removes what the copy holds.
        &[
            "-c",
            "user.name=xtask",
            "-c",
            "user.email=xtask@localhost",
            "-c",
            "maintenance.auto=false",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "--message=The repository, before the check changes it",
        ],
    ] {
        let status = Command::new("git").args(args).current_dir(copy).status()?;
        if !status.success() {
            return Err(io::Error::other(format!("git {} failed", args.join(" "))));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::repo;

    /// A copy named `name` that holds `bytes` bytes and was made `made` seconds into time.
    fn copy(name: &str, bytes: u64, made: u64) -> Held {
        Held {
            directory: PathBuf::from(name),
            bytes,
            made: SystemTime::UNIX_EPOCH + Duration::from_secs(made),
        }
    }

    fn copies() -> Vec<Held> {
        vec![
            copy("teams", 30, 3),
            copy("personal", 30, 1),
            copy("modules", 30, 2),
            copy("files", 30, 4),
        ]
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn ops_21_the_bound_is_the_one_that_ops_21_gives() {
        let repo = repo::Repo::load().unwrap();
        let rules = &repo.file("docs/standards/operations.md").unwrap().text;

        let given = rules
            .split_once("while the copies hold more than ")
            .and_then(|(_, rest)| rest.split_once(" gibibytes together"))
            .and_then(|(number, _)| number.parse::<u64>().ok());

        assert_eq!(given.map(|gibibytes| gibibytes << 30), Some(BOUND));
    }

    #[test]
    fn ops_21_the_copies_made_longest_ago_go_while_the_copies_hold_more_than_the_bound() {
        let making = Path::new("files");

        assert_eq!(beyond(copies(), making, 120), paths(&[]));
        assert_eq!(beyond(copies(), making, 119), paths(&["personal"]));
        assert_eq!(
            beyond(copies(), making, 60),
            paths(&["personal", "modules"])
        );
    }

    #[test]
    fn ops_21_the_copy_that_a_check_is_about_to_make_goes_last() {
        let making = Path::new("personal");

        assert_eq!(beyond(copies(), making, 60), paths(&["modules", "teams"]));
        assert_eq!(
            beyond(copies(), making, 29),
            paths(&["modules", "teams", "files", "personal"])
        );
    }

    #[test]
    fn ops_21_a_check_removes_the_copies_beyond_the_bound_with_their_builds() {
        // A tree of its own, under target/, which the build already ignores.
        let directory = "target/copies-test";
        repo::empty_directory(directory).unwrap();
        for (path, bytes) in [
            ("rename/rename/repo/justfile", 10),
            ("rename/rename/target/debug/deps/app", 20),
            ("new-module/modules/repo/justfile", 10),
            ("new-module/modules/target/debug/app", 20),
            // The repository's own build, and a file that is no copy, count for nothing.
            ("debug/deps/app", 100),
            ("rename/rename.log", 100),
        ] {
            repo::write(&format!("{directory}/{path}"), &"a".repeat(bytes)).unwrap();
        }
        let target = root().join(directory);
        let renamed = target.join("rename/rename");
        let modules = target.join("new-module/modules");
        let held = |path: &str| repo::read(&target.join(path)).is_ok();

        assert_eq!(remove_beyond(&target, &renamed, 60).unwrap(), paths(&[]));
        assert!(held("new-module/modules/target/debug/app"));

        let removed = remove_beyond(&target, &renamed, 59).unwrap();
        assert_eq!(removed, [modules]);
        assert!(!held("new-module/modules/repo/justfile"));
        assert!(!held("new-module/modules/target/debug/app"));
        assert!(held("rename/rename/target/debug/deps/app"));

        let removed = remove_beyond(&target, &renamed, 29).unwrap();
        assert_eq!(removed, [renamed]);
        assert!(!held("rename/rename/repo/justfile"));
        assert!(!held("rename/rename/target/debug/deps/app"));
        assert!(held("debug/deps/app"));
        assert!(held("rename/rename.log"));
    }

    // part: removal
    #[test]
    fn ops_21_the_copies_of_check_removable_s_runs_count_against_the_bound_too() {
        // A tree of its own, under target/, which the build already ignores.
        let directory = "target/copies-of-runs-test";
        repo::empty_directory(directory).unwrap();
        for (path, bytes) in [
            ("removable/personal/repo/justfile", 10),
            ("removable/personal/target/debug/deps/app", 20),
            ("rename/rename/repo/justfile", 10),
        ] {
            repo::write(&format!("{directory}/{path}"), &"a".repeat(bytes)).unwrap();
        }
        let target = root().join(directory);
        let personal = target.join("removable/personal");
        let renamed = target.join("rename/rename");

        assert_eq!(remove_beyond(&target, &renamed, 40).unwrap(), paths(&[]));

        let removed = remove_beyond(&target, &renamed, 39).unwrap();
        assert_eq!(removed, [personal]);
        assert!(repo::read(&target.join("removable/personal/target/debug/deps/app")).is_err());
        assert!(repo::read(&target.join("rename/rename/repo/justfile")).is_ok());
    }
    // end part: removal

    #[test]
    fn ops_21_a_check_removes_the_copies_beyond_the_bound_before_it_makes_one() {
        // A repository of its own, under target/, which the build already ignores.
        let directory = "target/copies-of-test";
        repo::empty_directory(directory).unwrap();
        repo::write(&format!("{directory}/.gitignore"), "/target\n").unwrap();
        repo::write(&format!("{directory}/justfile"), "checks:\n").unwrap();
        let root = root().join(directory);
        run(&root, "git", &["init", "--quiet"], &[]).unwrap();

        let first = Copy::of(&root, Check::Rename, "rename", u64::MAX).unwrap();
        assert!(repo::read(&first.root.join("justfile")).is_ok());

        let second = Copy::of(&root, Check::NewModule, "modules", 0).unwrap();
        assert!(repo::read(&first.root.join("justfile")).is_err());
        assert!(repo::read(&second.root.join("justfile")).is_ok());
    }
}
