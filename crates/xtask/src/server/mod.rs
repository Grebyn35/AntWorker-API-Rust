//! `server-setup` and `server-check`, which the `just` recipes of the same names run on a
//! developer's machine (OPS-19). Both send deploy/ at the tag of a release, the latest unless one
//! is named, to a temporary directory on the server over SSH, as a deploy of that release runs
//! its files. `server-setup` asks for the settings and the secrets that
//! the settings' choices need, then runs deploy/server-setup.sh, which installs Docker, prepares
//! the deployment's user, and writes what was asked. `server-check` runs deploy.sh's check of a
//! release on the server, then checks from here what only the outside sees: where each of the
//! deployment's addresses points, and whether the server's ports 80 and 443 can be reached.
//! docs/operations/deploy.md describes both.
//!
//! The questions, the settings file they make, and what the server is sent are pure functions of
//! deploy/settings.env.example, the server's state and the answers, so tests cover them without a
//! server.

mod base64;
mod check;
mod questions;
mod settings;
mod setup;
mod terminal;

use std::{
    io,
    path::Path,
    process::{Command, Stdio},
};

pub use check::check;
pub use setup::setup;

use crate::deploy::{DEPLOYMENT, send_files};

/// Copies the files of deploy/ at the tag of the release `version` to a new temporary
/// directory on `host`, as a deploy of the release runs them, and returns its path there.
fn send_release(root: &Path, host: &str, version: &str) -> Result<String, String> {
    let unpack =
        r#"set -e; directory=$(mktemp -d); tar -x -C "$directory"; printf '%s\n' "$directory""#;
    let written = send_files(root, host, version, unpack)?;
    let directory = written.trim().to_owned();
    if !directory.starts_with('/') {
        return Err(format!("the server named no directory, but {directory:?}"));
    }
    Ok(directory)
}

/// Runs `command` in the shell of `host`, and returns what it wrote on stdout; its stderr goes to
/// this process's.
pub fn on_server_output(host: &str, command: &str) -> Result<String, String> {
    let output = Command::new("ssh")
        .arg(host)
        .arg(command)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("can't run ssh: {error}"))?;
    if !output.status.success() {
        return Err(format!("{command} failed on {host} ({})", output.status));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{command} wrote what isn't UTF-8"))
}

#[expect(
    clippy::disallowed_methods,
    reason = "the task runs as a synchronous program, with no async runtime to stall"
)]
fn read(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path)
}

/// deploy/settings.env.example, which the tests of the questions and of the settings read.
#[cfg(test)]
fn example() -> String {
    String::from_utf8(
        read(&crate::deploy::root().join("deploy/settings.env.example")).expect("the example"),
    )
    .expect("UTF-8")
}
