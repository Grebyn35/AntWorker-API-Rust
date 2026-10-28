//! `server-setup`: reads what the server holds, asks the questions, and sends the answers to
//! deploy/server-setup.sh on the server.

use std::{
    io::Write,
    path::Path,
    process::{Command, ExitCode, Stdio},
};

use super::{
    on_server_output,
    questions::{Prompt, Release, State, Terminal, ask, github_defaults},
    read, send_checkout,
};
use crate::deploy::{quoted, refuse, root};

/// The settings `IMAGE_REPOSITORY` and `SOURCE` for the repository that the checkout fetches from,
/// on GitHub, or none.
fn defaults(root: &Path) -> Vec<(String, String)> {
    let remote = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    match remote.as_deref().and_then(github_defaults) {
        Some((repository, source)) => vec![
            ("IMAGE_REPOSITORY".to_owned(), repository),
            ("SOURCE".to_owned(), source),
        ],
        None => Vec::new(),
    }
}

/// The file at `path` in the checkout's HEAD, which the files sent to the server come from too.
fn at_head(root: &Path, path: &str) -> Option<String> {
    Command::new("git")
        .args(["show", &format!("HEAD:{path}")])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Sets up the server that SSH reaches at `host`, as root or a user that may run sudo without a
/// password, or changes its deployment's settings and secrets.
pub fn setup(host: &str) -> ExitCode {
    let root = root();
    let (Some(example), Some(alertmanager)) = (
        at_head(&root, "deploy/settings.env.example"),
        at_head(&root, "deploy/observability/alertmanager/alertmanager.yaml"),
    ) else {
        return refuse("Can't read the release's files at HEAD.");
    };
    let release = Release {
        settings: &example,
        alertmanager: &alertmanager,
    };
    let state = send_checkout(&root, host)
        .and_then(|directory| {
            on_server_output(
                host,
                &format!("sh {}/server-setup.sh state", quoted(&directory)),
            )
        })
        .and_then(|text| State::parse(&text));
    let state = match state {
        Ok(state) => state,
        Err(error) => return refuse(&format!("Can't read what {host} holds: {error}")),
    };
    let mut terminal = Terminal;
    terminal.show(&format!(
        "Setting up {host}, whose deployment runs as {}. Enter keeps the value in brackets.",
        state.user
    ));
    let setup = match ask(&release, &state, &defaults(&root), &mut terminal, |path| {
        read(Path::new(path)).map_err(|error| format!("Can't read {path}: {error}"))
    }) {
        Ok(setup) => setup,
        Err(error) => return refuse(&error),
    };
    let directory = match send_checkout(&root, host) {
        Ok(directory) => directory,
        Err(error) => return refuse(&format!("Can't copy deploy/ to {host}: {error}")),
    };
    let mut ssh = match Command::new("ssh")
        .arg(host)
        .arg(format!("sh {}/server-setup.sh apply", quoted(&directory)))
        .stdin(Stdio::piped())
        .spawn()
    {
        Ok(ssh) => ssh,
        Err(error) => return refuse(&format!("Can't run ssh: {error}")),
    };
    let sent = ssh
        .stdin
        .take()
        .map(|mut input| input.write_all(setup.payload().as_bytes()));
    match (sent, ssh.wait()) {
        (Some(Ok(())), Ok(status)) if status.success() => ExitCode::SUCCESS,
        (_, Ok(status)) if !status.success() => ExitCode::FAILURE,
        _ => refuse(&format!("Can't send the setup to {host}.")),
    }
}
