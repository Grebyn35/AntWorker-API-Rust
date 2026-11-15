//! Cargo as this program starts it. `just` starts this program through `cargo run`, which sets
//! variables for it that describe its own package: its name, its version and its manifest. A
//! build script can ask Cargo to run it again when one of them changes, as ring's does, and Cargo
//! compares them as its own environment holds them. A Cargo that inherited them from this program
//! would so build that crate again, and every crate that depends on it, after a Cargo that `just`
//! started without them, and the next one that `just` started would build them once more. So this
//! program starts Cargo, and `just`, whose tasks start Cargo, without them.

use std::{ffi::OsStr, process::Command};

/// A command that runs Cargo: the one that runs this program, or the one on the path.
pub fn cargo() -> Command {
    command(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

/// A command that runs `program` without the variables that Cargo set for this program.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for (name, _) in std::env::vars_os() {
        if describes_package(&name) {
            command.env_remove(name);
        }
    }
    command
}

/// Whether `name` is a variable that `cargo run` sets to describe the package of the program it
/// runs.
fn describes_package(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name == "CARGO_MANIFEST_DIR"
            || name == "CARGO_MANIFEST_PATH"
            || name.starts_with("CARGO_PKG_")
    })
}
