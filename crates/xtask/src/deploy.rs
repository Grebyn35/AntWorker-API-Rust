//! `deploy`, `backup` and `restore`, which `just deploy`, `just backup` and `just restore` run on
//! a developer's machine: they run the reference deployment's deploy/deploy.sh on its server over
//! SSH (OPS-16, OPS-17). A deploy takes a release by its version: it refuses a version that no
//! release has, sends the server the deployment's files from the release's tag, and runs that
//! release's deploy.sh, which verifies the release's images before it pulls them. The server
//! needs no checkout of the repository. docs/operations/deploy.md describes the deployment.
//!
//! Which release a version names is a pure function, so tests cover it without a server; the
//! tasks run git and ssh as the developer's own.

use std::{
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

use crate::release::Version;

/// The deployment's directory on the server, in the home of the user SSH signs in as.
const DEPLOYMENT: &str = "$HOME/deployment";

/// The version of the release that `requested` names, as its tag `v<version>` and its images
/// write it, given whether a tag exists; or why it can't be deployed.
pub fn release(requested: &str, tagged: impl Fn(&str) -> bool) -> Result<String, String> {
    let Ok(version) = requested.parse::<Version>() else {
        return Err(format!(
            "{requested} isn't a version. A deploy takes a release's version, X.Y.Z, such as \
             1.4.0, or X.Y.Z-rc.N for a prerelease."
        ));
    };
    if tagged(&format!("v{version}")) {
        return Ok(version.to_string());
    }
    Err(format!(
        "No release has the version {version}, so there is nothing to deploy. A deploy takes a \
         release, which `just release` makes from main once CI passes: \
         docs/operations/release.md says how."
    ))
}

/// `text` as one word of a POSIX shell's command, in single quotes.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Deploys the release `version` to the deployment that SSH reaches at `host`.
pub fn deploy(host: &str, version: &str, allow_restart: bool) -> ExitCode {
    let root = root();
    if let Err(error) = run(&root, "git", &["fetch", "--quiet", "--tags", "origin"]) {
        return refuse(&format!(
            "Can't fetch the release tags from GitHub: {error}"
        ));
    }
    let tagged = |tag: &str| {
        run(
            &root,
            "git",
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/tags/{tag}"),
            ],
        )
        .is_ok()
    };
    let version = match release(version, tagged) {
        Ok(version) => version,
        Err(refused) => return refuse(&refused),
    };
    let release = format!("{DEPLOYMENT}/releases/{version}");
    if let Err(error) = send_files(&root, host, &version, &release) {
        return refuse(&format!(
            "Can't copy the release's files to {host}: {error}"
        ));
    }
    let mut command = format!(
        "DEPLOYMENT={DEPLOYMENT} sh {release}/deploy.sh deploy {}",
        quoted(&version)
    );
    if allow_restart {
        command.push_str(" --allow-restart");
    }
    on_server(host, &command)
}

/// Takes a full base backup of the deployment that SSH reaches at `host`.
pub fn backup(host: &str) -> ExitCode {
    on_server(
        host,
        &format!("DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh backup"),
    )
}

/// Restores the database of the deployment that SSH reaches at `host`, as it stood at `moment`,
/// or at the latest moment its backups hold.
pub fn restore(host: &str, moment: Option<&str>) -> ExitCode {
    let mut command = format!("DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh restore");
    if let Some(moment) = moment.filter(|moment| !moment.is_empty()) {
        command.push(' ');
        command.push_str(&quoted(moment));
    }
    on_server(host, &command)
}

/// Copies the files of deploy/ at the tag of `version` to the directory `release` on `host`,
/// replacing what it held.
fn send_files(root: &Path, host: &str, version: &str, release: &str) -> Result<(), String> {
    let mut archive = Command::new("git")
        .args(["archive", "--format=tar", &format!("v{version}:deploy")])
        .current_dir(root)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't run git archive: {error}"))?;
    let stdout = archive.stdout.take().ok_or("git archive has no output")?;
    let unpacked = Command::new("ssh")
        .arg(host)
        .arg(format!(
            "set -e; rm -rf {release}; mkdir -p {release}; tar -x -C {release}"
        ))
        .stdin(Stdio::from(stdout))
        .status()
        .map_err(|error| format!("can't run ssh: {error}"))?;
    let archived = archive
        .wait()
        .map_err(|error| format!("git archive failed: {error}"))?;
    if !archived.success() {
        return Err("git archive failed".to_owned());
    }
    if !unpacked.success() {
        return Err(format!("unpacking them on the server failed ({unpacked})"));
    }
    Ok(())
}

/// Runs `command` in the shell of `host` through SSH, with this process's input and output, and
/// exits as it does.
fn on_server(host: &str, command: &str) -> ExitCode {
    match Command::new("ssh").arg(host).arg(command).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => refuse(&format!("Can't run ssh: {error}")),
    }
}

/// Runs `program` with `args` in `root`, and returns what it wrote on stdout, or why it failed.
fn run(root: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("can't run {program}: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[expect(clippy::print_stderr, reason = "the refusal is the task's output")]
fn refuse(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_16_a_version_that_no_release_has_is_refused_with_a_pointer_to_releases() {
        let refused = release("1.4.0", |_| false).unwrap_err();

        assert!(
            refused.contains("No release has the version 1.4.0"),
            "{refused}"
        );
        assert!(refused.contains("docs/operations/release.md"), "{refused}");
    }

    #[test]
    fn ops_16_a_released_version_is_deployed_and_a_prerelease_too() {
        assert_eq!(
            release("1.4.0", |tag| tag == "v1.4.0").as_deref(),
            Ok("1.4.0")
        );
        assert_eq!(
            release("1.4.0-rc.2", |tag| tag == "v1.4.0-rc.2").as_deref(),
            Ok("1.4.0-rc.2")
        );
    }

    #[test]
    fn ops_16_a_version_written_as_its_tag_deploys_the_release_of_the_version() {
        assert_eq!(
            release("v1.4.0", |tag| tag == "v1.4.0").as_deref(),
            Ok("1.4.0")
        );
    }

    #[test]
    fn ops_16_what_isn_t_a_version_is_refused() {
        for requested in ["latest", "1.4", "1.4.0.1", ""] {
            let refused = release(requested, |_| true).unwrap_err();
            assert!(
                refused.contains("isn't a version"),
                "{requested}: {refused}"
            );
        }
    }

    #[test]
    fn ops_17_a_moment_reaches_the_server_s_shell_as_one_word() {
        assert_eq!(quoted("2026-10-25 10:41:31+00"), "'2026-10-25 10:41:31+00'");
        assert_eq!(quoted("it's"), r"'it'\''s'");
    }
}
