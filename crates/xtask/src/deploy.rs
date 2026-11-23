//! `deploy`, `backup`, `restore`, `status`, `logs`, `replace-key`, `remove-second-factor` and
//! `end-sessions`, which the `just` recipes of the same names run on a developer's machine: they
//! run the reference deployment's deploy/deploy.sh on its server over SSH (OPS-14, OPS-15,
//! OPS-19, AUTH-072, AUTH-094). A deploy takes a release by its version: it refuses
//! a version that no release has, sends the server the deployment's files from the release's tag,
//! and runs that release's deploy.sh, which verifies the release's images before it pulls them.
//! The other tasks run the deploy.sh of the release that the last deploy installed. The server
//! needs no checkout of the repository. docs/operations/deploy.md describes the deployment.
//!
//! Which release a version names is a pure function, so tests cover it without a server; the
//! tasks run git and ssh as the developer's own.
// part: workspace_setup
//! `setup-workspace` runs deploy.sh the same way (WORKSPACES-023).
// end part: workspace_setup
// part: superadmins
//! `superadmin` runs deploy.sh the same way (SUPERADMINS-001, SUPERADMINS-008).
// end part: superadmins
// part: files
//! `restore-files` runs deploy.sh the same way (FILES-028).
// end part: files

use std::{
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

use crate::{
    release::{Version, releases},
    restore::{bucket_time, planned},
    server::on_server_output,
};

/// The deployment's directory on the server, in the home of the user SSH signs in as.
pub const DEPLOYMENT: &str = "$HOME/deployment";

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
pub fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The version of the release that `requested` names, or of the latest release, once the
/// release tags are fetched from GitHub; or why no release can be taken.
pub fn released(root: &Path, requested: Option<&str>) -> Result<String, String> {
    git(root, &["fetch", "--quiet", "--tags", "origin"])
        .map_err(|error| format!("Can't fetch the release tags from GitHub: {error}"))?;
    chosen(requested, &releases(root)?, |tag| {
        git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/tags/{tag}"),
            ],
        )
        .is_ok()
    })
}

/// The release that a task on a server takes: the one `requested` names, as [`release`] decides
/// it, given whether a tag exists, or else the latest of `releases`, a prerelease included; or
/// why none can be taken.
fn chosen(
    requested: Option<&str>,
    releases: &[Version],
    tagged: impl Fn(&str) -> bool,
) -> Result<String, String> {
    match requested {
        Some(requested) => release(requested, tagged),
        None => releases.iter().max().map(ToString::to_string).ok_or_else(|| {
            "No release exists yet, and a task on a server takes a release. `just release` makes \
             one: docs/operations/release.md says how."
                .to_owned()
        }),
    }
}

/// Deploys the release `requested`, or the latest release, to the deployment that SSH reaches at
/// `host`.
pub fn deploy(host: &str, requested: Option<&str>, allow_restart: bool) -> ExitCode {
    let root = root();
    let version = match released(&root, requested) {
        Ok(version) => version,
        Err(refused) => return refuse(&refused),
    };
    if requested.is_none() {
        note(&format!("Deploying {version}, the latest release."));
    }
    let release = format!("{DEPLOYMENT}/releases/{version}");
    let unpack = format!("set -e; rm -rf {release}; mkdir -p {release}; tar -x -C {release}");
    if let Err(error) = send_files(&root, host, &version, &unpack) {
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
/// or at the latest moment its backups hold. It first reads what the backup bucket records, and
/// plans the backup the restore starts from and the timeline it replays along. With
/// `allow_unarchived`, it restores although the database's log doesn't reach the bucket. With
/// `bucket_as_of`, it reads what the bucket recorded at that time, and restores from the backups
/// that it held then (OPS-22).
pub fn restore(
    host: &str,
    moment: Option<&str>,
    allow_unarchived: bool,
    bucket_as_of: Option<&str>,
) -> ExitCode {
    let moment = moment.filter(|moment| !moment.is_empty());
    let held = match bucket_as_of.map(|time| bucket_time(time).ok_or(time)) {
        None => None,
        Some(Ok(held)) => Some(held),
        Some(Err(time)) => {
            return refuse(&format!(
                "{time} isn't a time. A time is a date and a time with its offset from UTC, as \
                 Postgres writes one, such as 2026-10-25 10:41:31+00."
            ));
        }
    };
    let mut inputs = format!("DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh restore-inputs");
    if let Some(held) = &held {
        inputs.push(' ');
        inputs.push_str(&quoted(held));
    }
    let inputs = match on_server_output(host, &inputs) {
        Ok(inputs) => inputs,
        Err(error) => {
            return refuse(&format!(
                "Can't read what the backup bucket records: {error}"
            ));
        }
    };
    let plan = match planned(moment, held.as_deref(), &inputs) {
        Ok(plan) => plan,
        Err(refused) => return refuse(&refused),
    };
    let source = match &held {
        Some(held) => format!("the backup {}, which the bucket held at {held}", plan.set),
        None => format!("the backup {}", plan.set),
    };
    note(&match moment {
        Some(moment) => format!(
            "Restoring from {source}, along timeline {}, the one live at {moment}.",
            plan.timeline
        ),
        None => format!(
            "Restoring the latest state from {source}, along timeline {}.",
            plan.timeline
        ),
    });
    let mut command = format!("DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh restore");
    for argument in [moment.unwrap_or(""), &plan.set, &plan.timeline.to_string()] {
        command.push(' ');
        command.push_str(&quoted(argument));
    }
    if allow_unarchived {
        command.push_str(" --allow-unarchived");
    }
    if let Some(held) = &held {
        command.push_str(" --bucket-as-of ");
        command.push_str(&quoted(held));
    }
    on_server(host, &command)
}

/// Shows the state of the deployment that SSH reaches at `host`.
pub fn status(host: &str) -> ExitCode {
    on_server(
        host,
        &format!("DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh status"),
    )
}

/// Removes the authenticator app of the account that holds `email` on the deployment that SSH
/// reaches at `host`, through the API's subcommand `remove-second-factor` (AUTH-072).
pub fn remove_second_factor(host: &str, email: &str) -> ExitCode {
    on_server(
        host,
        &format!(
            "DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh remove-second-factor {}",
            quoted(email)
        ),
    )
}

/// Writes a new key in place of `key`, one of the two that a deploy generated on the deployment
/// that SSH reaches at `host`, which refuses any other name (OPS-19).
pub fn replace_key(host: &str, key: &str) -> ExitCode {
    on_server(
        host,
        &format!(
            "DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh replace-key {}",
            quoted(key)
        ),
    )
}

/// Ends every session of the account that holds the address `whose` on the deployment that SSH
/// reaches at `host`, or, when `whose` is `--all`, of every account, through the API's subcommand
/// `end-sessions` (AUTH-094).
pub fn end_sessions(host: &str, whose: &str) -> ExitCode {
    on_server(
        host,
        &format!(
            "DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh end-sessions {}",
            quoted(whose)
        ),
    )
}

// part: files
/// Writes again, on the deployment that SSH reaches at `host`, each object of the files bucket
/// whose version at `moment` isn't its current one, through the API's subcommand `restore-files`
/// (FILES-028), which takes the moment in UTC. `moment` is a time with its offset from UTC, as
/// a restore takes one; what isn't one is refused here.
pub fn restore_files(host: &str, moment: &str) -> ExitCode {
    let Some(at) = crate::restore::utc_moment(moment) else {
        return refuse(&format!(
            "{moment} isn't a moment. A moment is a date and a time with its offset from UTC, as \
             Postgres writes one, such as 2026-10-25 10:41:31+00."
        ));
    };
    on_server(
        host,
        &format!(
            "DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh restore-files {}",
            quoted(&at)
        ),
    )
}
// end part: files

// part: superadmins
/// Runs the API's subcommand `superadmin` with `arguments` on the deployment that SSH reaches at
/// `host`: `add` or `remove` with an account's address, which makes the account a superadmin or
/// ends its being one, or `list`, which shows the deployment's superadmins (SUPERADMINS-001,
/// SUPERADMINS-003, SUPERADMINS-008).
pub fn superadmin(host: &str, arguments: &[&str]) -> ExitCode {
    let mut command = format!("DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh superadmin");
    for argument in arguments {
        command.push(' ');
        command.push_str(&quoted(argument));
    }
    on_server(host, &command)
}
// end part: superadmins

// part: workspace_setup
/// Creates the workspace `name` on the deployment that SSH reaches at `host`, owned by the
/// account that holds `email`, which the run creates, named `owner_name`, when, with email on,
/// no account holds the address, through the API's subcommand `setup-workspace`
/// (WORKSPACES-023).
pub fn setup_workspace(host: &str, name: &str, email: &str, owner_name: Option<&str>) -> ExitCode {
    let mut command = format!(
        "DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh setup-workspace {} {}",
        quoted(name),
        quoted(email)
    );
    if let Some(owner_name) = owner_name {
        command.push(' ');
        command.push_str(&quoted(owner_name));
    }
    on_server(host, &command)
}
// end part: workspace_setup

/// Shows the logs of the deployment's services that `arguments` name, or of all of them, and
/// with `--follow` among them, the lines that follow.
pub fn logs(host: &str, arguments: &[String]) -> ExitCode {
    let mut command = format!("DEPLOYMENT={DEPLOYMENT} sh {DEPLOYMENT}/deploy.sh logs");
    for argument in arguments {
        command.push(' ');
        command.push_str(&quoted(argument));
    }
    // A terminal on the server, so that ending ssh ends the logs that follow there.
    let follows = arguments.iter().any(|argument| argument == "--follow");
    on_server_with(host, &command, follows && io::stdin().is_terminal())
}

/// Sends the files of deploy/ at the tag of `version` to `host`, as a tar archive on the input of
/// `unpack`, a command of the server's shell that writes them out, and returns what `unpack`
/// wrote on its standard output. Its standard error goes to this process's.
pub fn send_files(root: &Path, host: &str, version: &str, unpack: &str) -> Result<String, String> {
    let mut archive = archive(root, &format!("v{version}:deploy"))
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't run git archive: {error}"))?;
    let stdout = archive.stdout.take().ok_or("git archive has no output")?;
    let unpacked = Command::new("ssh")
        .arg(host)
        .arg(unpack)
        .stdin(Stdio::from(stdout))
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("can't run ssh: {error}"))?;
    let archived = archive
        .wait()
        .map_err(|error| format!("git archive failed: {error}"))?;
    if !archived.success() {
        return Err("git archive failed".to_owned());
    }
    if !unpacked.status.success() {
        return Err(format!(
            "unpacking them on the server failed ({})",
            unpacked.status
        ));
    }
    Ok(String::from_utf8_lossy(&unpacked.stdout).into_owned())
}

/// The command that writes the files of `tree` in `root` as a tar archive, as the repository
/// holds them. The archive of a subtree, such as deploy/ at a tag, doesn't read the root's
/// .gitattributes, so the developer's core.autocrlf would otherwise apply, which Git for
/// Windows sets: it would end each line of the shell scripts with a carriage return.
pub fn archive(root: &Path, tree: &str) -> Command {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.autocrlf=false", "archive", "--format=tar", tree])
        .current_dir(root);
    command
}

/// Runs `command` in the shell of `host` through SSH, with this process's input and output, and
/// exits as it does.
fn on_server(host: &str, command: &str) -> ExitCode {
    on_server_with(host, command, false)
}

/// Runs `command` as [`on_server`] does, and with `terminal`, in a terminal on the server, whose
/// processes end when ssh does.
fn on_server_with(host: &str, command: &str, terminal: bool) -> ExitCode {
    let mut ssh = Command::new("ssh");
    if terminal {
        ssh.arg("-t");
    }
    match ssh.arg(host).arg(command).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => refuse(&format!("Can't run ssh: {error}")),
    }
}

/// Runs git with `args` in `root`, and returns what it wrote on stdout, or why it failed.
fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("can't run git: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[expect(clippy::print_stderr, reason = "the refusal is the task's output")]
pub fn refuse(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[expect(clippy::print_stdout, reason = "the note is the task's output")]
fn note(message: &str) {
    println!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_14_a_version_that_no_release_has_is_refused_with_a_pointer_to_releases() {
        let refused = release("1.4.0", |_| false).unwrap_err();

        assert!(
            refused.contains("No release has the version 1.4.0"),
            "{refused}"
        );
        assert!(refused.contains("docs/operations/release.md"), "{refused}");
    }

    #[test]
    fn ops_14_a_released_version_is_deployed_and_a_prerelease_too() {
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
    fn ops_14_a_version_written_as_its_tag_deploys_the_release_of_the_version() {
        assert_eq!(
            release("v1.4.0", |tag| tag == "v1.4.0").as_deref(),
            Ok("1.4.0")
        );
    }

    #[test]
    fn ops_14_what_isn_t_a_version_is_refused() {
        for requested in ["latest", "1.4", "1.4.0.1", ""] {
            let refused = release(requested, |_| true).unwrap_err();
            assert!(
                refused.contains("isn't a version"),
                "{requested}: {refused}"
            );
        }
    }

    #[test]
    fn ops_14_a_deploy_takes_the_latest_release_unless_it_names_one() {
        let releases: Vec<Version> = ["0.9.0", "1.0.0-rc.2", "1.0.0", "1.1.0-rc.1"]
            .iter()
            .filter_map(|version| version.parse().ok())
            .collect();

        let latest = chosen(None, &releases, |_| false);
        let named = chosen(Some("0.9.0"), &releases, |tag| tag == "v0.9.0");
        let none = chosen(None, &[], |_| false);

        assert_eq!(latest.as_deref(), Ok("1.1.0-rc.1"));
        assert_eq!(named.as_deref(), Ok("0.9.0"));
        assert!(
            none.as_ref()
                .is_err_and(|refused| refused.contains("No release exists yet")),
            "{none:?}"
        );
    }

    #[test]
    fn ops_14_the_files_a_deploy_sends_keep_the_line_endings_the_repository_holds() {
        let output = archive(&root(), "HEAD:deploy")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.autocrlf")
            .env("GIT_CONFIG_VALUE_0", "true")
            .output()
            .expect("run git archive");

        assert!(output.status.success(), "{output:?}");
        assert!(
            !output.stdout.windows(2).any(|pair| pair == b"\r\n"),
            "a file of deploy/ arrives with Windows line endings"
        );
    }

    #[test]
    fn ops_15_a_moment_reaches_the_server_s_shell_as_one_word() {
        assert_eq!(quoted("2026-10-25 10:41:31+00"), "'2026-10-25 10:41:31+00'");
        assert_eq!(quoted("it's"), r"'it'\''s'");
    }

    /// What `read_settings`, in deploy/tasks/common.sh, refuses of the settings, which a deploy
    /// reads before it starts a service. The tests run the script in a POSIX shell, as the server
    /// does, which Windows doesn't provide, so they compile only on Unix.
    #[cfg(unix)]
    mod settings {
        use super::*;

        /// The settings that `read_settings` requires, with a bundled database and no telemetry.
        const REQUIRED: &[&str] = &[
            "API_ADDRESS=api.example.com",
            // part: files
            "FILES_S3_ENDPOINT=files.example.com",
            "FILES_S3_REGION=region",
            "FILES_S3_BUCKET=files",
            // end part: files
            "BACKUP_S3_ENDPOINT=backups.example.com",
            "BACKUP_S3_REGION=region",
            "BACKUP_S3_BUCKET=backups",
            "TELEMETRY=off",
            "EMAIL=off",
        ];

        /// What `read_settings` writes to stderr as it refuses settings.env holding the required
        /// settings, then `settings`, whose values replace theirs, or `None` when it takes them.
        fn refusal(settings: &[&str]) -> Option<String> {
            refusal_of("read_settings", settings)
        }

        /// What the functions of common.sh that `steps` runs write to stderr as they refuse
        /// settings.env holding the required settings, then `settings`, or `None` when they take
        /// them.
        fn refusal_of(steps: &str, settings: &[&str]) -> Option<String> {
            refusal_with(&[], steps, settings)
        }

        /// As `refusal_of`, in an environment that holds `PATH` and `variables` alone.
        fn refusal_with(
            variables: &[(&str, &str)],
            steps: &str,
            settings: &[&str],
        ) -> Option<String> {
            let lines: Vec<&str> = REQUIRED.iter().chain(settings).copied().collect();
            let output = Command::new("sh")
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .envs(variables.iter().copied())
                .args([
                    "-c",
                    "deployment=$(mktemp -d) && printf '%s\\n' \"$1\" > \"$deployment/settings.env\" \
                     && . \"$2\" && (eval \"$3\"); refused=$?; rm -rf \"$deployment\"; \
                     exit $refused",
                    "sh",
                ])
                .arg(lines.join("\n"))
                .arg(root().join("deploy/tasks/common.sh"))
                .arg(steps)
                .output()
                .unwrap();
            (!output.status.success()).then(|| String::from_utf8_lossy(&output.stderr).into_owned())
        }

        /// Asserts that `read_settings` refuses the setting `name` set to each of `refused`,
        /// with a message that names it, and takes it set to each of `taken`.
        fn refuses(name: &str, refused: &[&str], taken: &[&str]) {
            for value in refused {
                let setting = format!("{name}={value}");
                let message = refusal(&[setting.as_str()]);
                assert!(
                    message
                        .as_deref()
                        .is_some_and(|message| message.contains(&format!("deploy: {name} is "))),
                    "{setting}: {message:?}"
                );
            }
            for value in taken {
                let setting = format!("{name}={value}");
                assert_eq!(refusal(&[setting.as_str()]), None, "{setting}");
            }
        }

        #[test]
        fn ops_14_a_deploy_refuses_a_port_and_a_uri_style_of_the_backups_bucket_that_its_type_line_excludes()
         {
            refuses(
                "BACKUP_S3_PORT",
                &["0", "65536", "443x", "-1"],
                &["", "1", "443", "65535"],
            );
            refuses(
                "BACKUP_S3_URI_STYLE",
                &["virtual", "Host", "host path"],
                &["", "host", "path"],
            );
            // The Type line applies them with DATABASE=bundled alone.
            assert_eq!(
                refusal(&[
                    "DATABASE=managed",
                    "BACKUP_S3_PORT=443x",
                    "BACKUP_S3_URI_STYLE=virtual"
                ]),
                None
            );
        }

        /// Asserts that `read_settings` refuses settings.env holding the required settings, then
        /// each line of `refused`, with a message that names that line, and takes it with each
        /// line of `taken`.
        fn refuses_lines(refused: &[&str], taken: &[&str]) {
            let named = format!("deploy: line {} of settings.env", REQUIRED.len() + 1);
            for line in refused {
                let message = refusal(&[*line]);
                assert!(
                    message
                        .as_deref()
                        .is_some_and(|message| message.contains(&named)),
                    "{line:?}: {message:?}"
                );
            }
            for line in taken {
                assert_eq!(refusal(&[*line]), None, "{line:?}");
            }
        }

        #[test]
        fn ops_17_a_deploy_refuses_unencrypted_email_to_another_server_however_its_line_writes_it()
        {
            let remote = [
                "EMAIL=smtp",
                "SMTP_HOST=192.0.2.25",
                "EMAIL_FROM=a@example.com",
            ];
            let unencrypted = [
                r#"SMTP_SECURITY="none""#,
                "SMTP_SECURITY='none'",
                "SMTP_SECURITY=none # unencrypted",
                "SMTP_SECURITY=none ",
                "SMTP_SECURITY= none",
            ];

            let encrypted = refusal(&[&remote[..], &["SMTP_SECURITY=starttls"]].concat());
            let refusals =
                unencrypted.map(|security| refusal(&[&remote[..], &[security]].concat()));

            assert_eq!(encrypted, None);
            for (security, refused) in unencrypted.iter().zip(&refusals) {
                assert!(refused.is_some(), "{security:?}");
            }
        }

        #[test]
        fn ops_14_a_deploy_refuses_a_quoted_value_of_settings_env() {
            refuses_lines(
                &[
                    r#"SMTP_SECURITY="none""#,
                    "SMTP_SECURITY='none'",
                    r#"SMTP_SECURITY="none" # a comment"#,
                    r#"EMAIL_FROM="no\nne""#,
                ],
                &[r#"SMTP_USERNAME=no"ne"#, "SMTP_USERNAME=it's"],
            );
        }

        #[test]
        fn ops_14_a_deploy_refuses_a_comment_after_a_value_of_settings_env_and_takes_a_hash_within_one()
         {
            refuses_lines(
                &["SMTP_SECURITY=none # a comment", "SMTP_SECURITY=none  #"],
                &["SMTP_USERNAME=none#x", "SMTP_USERNAME=#x"],
            );
        }

        #[test]
        fn ops_14_a_deploy_refuses_a_space_at_either_end_of_a_value_of_settings_env() {
            refuses_lines(
                &[
                    "SMTP_SECURITY=none ",
                    "SMTP_SECURITY= none",
                    "SMTP_SECURITY= ",
                ],
                &["EMAIL_FROM=AntWorker <noreply@example.com>"],
            );
        }

        #[test]
        fn ops_14_a_deploy_refuses_a_line_of_settings_env_that_isn_t_empty_a_comment_or_a_name_and_its_value()
         {
            refuses_lines(
                &[
                    "export SMTP_SECURITY=none",
                    " SMTP_SECURITY=none",
                    "SMTP_SECURITY =none",
                    "SMTP_SECURITY: none",
                    "SMTP_SECURITY",
                    "smtp_security=none",
                    "1SMTP_SECURITY=none",
                    " ",
                    " # a comment",
                ],
                &["", "# a comment", "#SMTP_SECURITY=none"],
            );
        }

        #[test]
        fn ops_14_a_deploy_refuses_a_value_of_settings_env_with_a_control_character_a_dollar_or_a_backslash()
         {
            refuses_lines(
                &[
                    "SMTP_SECURITY=none\r",
                    "SMTP_USERNAME=a\tb",
                    "SMTP_USERNAME=a\u{7f}b",
                    "SMTP_USERNAME=a$b",
                    "SMTP_USERNAME=$HOME",
                    r"SMTP_USERNAME=a\b",
                ],
                &["SMTP_USERNAME=~a"],
            );
        }

        #[test]
        fn ops_14_a_deploy_refuses_a_unicode_space_at_either_end_of_a_value_of_settings_env_and_takes_a_letter_beyond_ascii_there()
         {
            // Each character beyond ASCII that Unicode lists as white space, and of its one
            // range of them the first, one within and the last.
            let spaces = [
                '\u{85}', '\u{a0}', '\u{1680}', '\u{2000}', '\u{2003}', '\u{200a}', '\u{2028}',
                '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}',
            ];
            let refused: Vec<String> = spaces
                .iter()
                .flat_map(|space| {
                    [
                        format!("SMTP_USERNAME=none{space}"),
                        format!("SMTP_USERNAME={space}none"),
                    ]
                })
                .collect();
            let refused: Vec<&str> = refused.iter().map(String::as_str).collect();

            refuses_lines(
                &refused,
                &[
                    "EMAIL_FROM=\u{c5}sa <noreply@example.com>",
                    "EMAIL_FROM=S\u{f6}kning <noreply@example.com>",
                    "SMTP_USERNAME=caf\u{e9}",
                    // A space of Unicode's within a value, and the characters whose bytes
                    // stand beside the spaces': U+00A1, U+200B and U+3001.
                    "SMTP_USERNAME=a\u{a0}b",
                    "SMTP_USERNAME=a\u{a1}",
                    "SMTP_USERNAME=\u{a1}a",
                    "SMTP_USERNAME=a\u{200b}",
                    "SMTP_USERNAME=\u{200b}a",
                    "SMTP_USERNAME=a\u{3001}",
                ],
            );
        }

        #[test]
        fn ops_14_a_deploy_refuses_an_environment_that_sets_a_setting_of_settings_env() {
            let set_twice = refusal_with(
                &[("SMTP_SECURITY", "none")],
                "read_settings",
                &["SMTP_SECURITY=starttls"],
            );
            let set_empty = refusal_with(
                &[("SMTP_SECURITY", "")],
                "read_settings",
                &["SMTP_SECURITY="],
            );
            // A variable that no line of the file sets, and one whose name only ends as a
            // line's does.
            let beside = refusal_with(
                &[("SMTP_HOST", "192.0.2.25"), ("MY_SMTP_SECURITY", "none")],
                "read_settings",
                &["SMTP_SECURITY=starttls", "#SMTP_HOST=localhost"],
            );

            for refused in [set_twice, set_empty] {
                assert!(
                    refused.as_deref().is_some_and(|message| message.contains(
                        "deploy: the environment that this task runs in sets SMTP_SECURITY"
                    )),
                    "{refused:?}"
                );
            }
            assert_eq!(beside, None);
        }

        #[test]
        fn ops_14_a_deploy_takes_the_lines_of_settings_env_that_compose_and_its_checks_read_alike()
        {
            let settings = [
                "",
                "# The mail server, a comment.",
                "SMTP_PORT=",
                "SMTP_USERNAME=first",
                "SMTP_USERNAME=second",
                "CORS_ALLOWED_ORIGINS=https://app.example.com,https://admin.example.com",
                "TRUSTED_PROXIES=192.0.2.0/24 2001:db8::/32",
                "MANAGED_GRAFANA_URL=https://stack.grafana.net/a?b=c&d=e",
            ];

            assert_eq!(refusal(&settings), None);
        }

        #[test]
        fn ops_14_every_line_of_the_settings_example_is_of_a_form_that_a_deploy_takes() {
            let example = String::from_utf8(
                crate::repo::read(&root().join("deploy/settings.env.example"))
                    .expect("read the example"),
            )
            .expect("UTF-8");
            let lines: Vec<&str> = example.lines().collect();

            assert_eq!(refusal_of("settings_lines", &lines), None);
        }

        /// The names of the settings that deploy/compose.yaml requires, in a `${NAME:?message}`
        /// or a `${NAME?message}` whose message names settings.env. One whose message names
        /// neither settings.env nor state/deploy.env, the file a deploy writes, fails.
        fn required_by_compose() -> Vec<String> {
            let compose = String::from_utf8(
                crate::repo::read(&root().join("deploy/compose.yaml"))
                    .expect("read deploy/compose.yaml"),
            )
            .expect("UTF-8");
            let mut required = Vec::new();
            for interpolation in compose.split("${").skip(1) {
                let name: String = interpolation
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                let rest = &interpolation[name.len()..];
                let Some(message) = rest.strip_prefix(":?").or_else(|| rest.strip_prefix('?'))
                else {
                    continue;
                };
                let message = message.split('}').next().unwrap_or_default();
                if message.contains("settings.env") {
                    required.push(name);
                } else {
                    assert!(
                        message.contains("state/deploy.env"),
                        "${{{name}?{message}}}"
                    );
                }
            }
            required
        }

        #[test]
        fn ops_19_the_check_refuses_settings_env_without_each_setting_that_compose_requires_of_it()
        {
            let required = required_by_compose();

            let refusals: Vec<(String, Option<String>)> = ["bundled", "managed"]
                .iter()
                .flat_map(|database| required.iter().map(move |name| (*database, name)))
                .map(|(database, name)| {
                    let settings = [format!("DATABASE={database}"), format!("{name}=")];
                    let lines: Vec<&str> = settings.iter().map(String::as_str).collect();
                    (format!("{database}: {name}"), refusal(&lines))
                })
                .collect();

            assert!(!required.is_empty());
            for (case, refused) in &refusals {
                let name = case.rsplit(' ').next().unwrap_or_default();
                assert!(
                    refused
                        .as_deref()
                        .is_some_and(|message| message.contains(&format!("deploy: {name} is "))),
                    "{case}: {refused:?}"
                );
            }
        }

        #[test]
        fn ops_14_a_deploy_refuses_an_email_setting_that_its_type_line_excludes() {
            refuses("EMAIL", &["", "sendmail", "SMTP"], &["off", "smtp"]);
        }

        #[test]
        fn ops_14_a_deploy_refuses_a_whole_number_with_a_leading_zero_or_beyond_the_shell_s_range()
        {
            refuses(
                "BACKUP_HOUR",
                &["03", "00", "99999999999999999999", "24"],
                &["", "0", "3", "23"],
            );
        }

        #[test]
        fn ops_18_a_deploy_refuses_connections_that_wouldn_t_fit_however_the_settings_write_their_numbers()
         {
            // 2 colors of 10 replicas, of 5 connections each, and 2 beside them: 102 of the 97
            // that 100 leaves, which a shell's arithmetic reads as 82 of 010.
            let settings = [
                "API_REPLICAS=010",
                "DATABASE_MAX_CONNECTIONS=4",
                "POSTGRES_MAX_CONNECTIONS=100",
            ];

            let refused = refusal_of("read_settings && check_connections", &settings);

            assert!(
                refused
                    .as_deref()
                    .is_some_and(|message| message.contains("deploy: API_REPLICAS is ")),
                "{refused:?}"
            );
        }

        // part: files
        #[test]
        fn ops_14_a_deploy_refuses_a_port_and_a_uri_style_of_the_files_bucket_that_its_type_line_excludes()
         {
            refuses(
                "FILES_S3_PORT",
                &["0", "65536", "443x", "-1"],
                &["", "1", "443", "65535"],
            );
            refuses(
                "FILES_S3_URI_STYLE",
                &["virtual", "Host", "host path"],
                &["", "host", "path"],
            );
        }
        // end part: files
    }
}
