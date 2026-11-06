//! The copies of the repository that a check changes: `check-removable` removes each run's parts
//! from one, and `check-new-module` adds a module to one, as a fork does, then each runs
//! `just checks` on it. Each copy is in `target/<check>/<name>/repo/`, with its build in
//! `target/<check>/<name>/target/`, so no copy's `cargo run` finds a binary that another built.

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

/// The repository's root.
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Runs `program` with `args` in `directory`, with `environment` added, and fails unless it
/// succeeds. It writes to the task's own output.
pub fn run(
    directory: &Path,
    program: &str,
    args: &[&str],
    environment: &[(&str, &str)],
) -> Result<(), String> {
    let status = Command::new(program)
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
/// so its tests use the same Postgres and Mailpit, whose test databases are named after their
/// migrations.
pub struct Copy {
    /// The copy's root.
    root: PathBuf,
    /// Its build directory.
    target: String,
    /// The repository's Compose project.
    project: String,
}

impl Copy {
    /// A fresh copy for the check `check`, named `name`, such as a run of `check-removable`.
    pub fn fresh(check: &str, name: &str) -> Result<Self, String> {
        let root = root();
        let directory = root.join("target").join(check).join(name);
        let copy = directory.join("repo");
        fresh_copy(&root, &copy).map_err(|error| format!("can't copy the repository: {error}"))?;
        Ok(Self {
            root: copy,
            target: directory.join("target").to_string_lossy().into_owned(),
            project: project(),
        })
    }

    /// Runs `just` with `args` on the copy, and fails unless it succeeds.
    pub fn just(&self, args: &[&str]) -> Result<(), String> {
        run(
            &self.root,
            "just",
            args,
            &[
                ("CARGO_TARGET_DIR", self.target.as_str()),
                ("COMPOSE_PROJECT_NAME", self.project.as_str()),
            ],
        )
    }
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
        &[
            "-c",
            "user.name=xtask",
            "-c",
            "user.email=xtask@localhost",
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
