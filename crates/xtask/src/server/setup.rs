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
    read, send_release,
};
use crate::deploy::{quoted, refuse, released, root};

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

/// The file at `path` at the tag of the release `version`, which the files sent to the server
/// come from too.
fn in_release(root: &Path, version: &str, path: &str) -> Option<String> {
    Command::new("git")
        .args(["show", &format!("v{version}:{path}")])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Sets up the server that SSH reaches at `host`, as root or a user that may run sudo without a
/// password, or changes its deployment's settings and secrets.
pub fn setup(host: &str, version: Option<&str>) -> ExitCode {
    let root = root();
    let version = match released(&root, version) {
        Ok(version) => version,
        Err(refused) => return refuse(&refused),
    };
    let (Some(example), Some(alertmanager)) = (
        in_release(&root, &version, "deploy/settings.env.example"),
        in_release(
            &root,
            &version,
            "deploy/observability/alertmanager/alertmanager.yaml",
        ),
    ) else {
        return refuse(&format!("Can't read the files of the release {version}."));
    };
    let release = Release {
        settings: &example,
        alertmanager: &alertmanager,
    };
    let state = send_release(&root, host, &version)
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
    let directory = match send_release(&root, host, &version) {
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
        (Some(Ok(())), Ok(status)) if status.success() => {
            terminal.show(&reaching(host, &resolved(host), &state.user));
            ExitCode::SUCCESS
        }
        (_, Ok(status)) if !status.success() => ExitCode::FAILURE,
        _ => refuse(&format!("Can't send the setup to {host}.")),
    }
}

/// What SSH makes of a destination: the address, the port and the user it signs in as.
#[derive(Debug, Default)]
struct Resolved {
    hostname: String,
    port: String,
    user: String,
}

/// What `ssh -G` resolves `host` to, through ~/.ssh/config, or nothing that it could resolve.
fn resolved(host: &str) -> Resolved {
    let output = Command::new("ssh").arg("-G").arg(host).output();
    let text = output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
            .unwrap_or_default()
            .to_owned()
    };
    Resolved {
        hostname: value("hostname"),
        port: value("port"),
        user: value("user"),
    }
}

/// How the other tasks reach the server that the setup reached at `host`, which SSH resolves to
/// `resolved`, and whose deployment runs as `user`: by `host` itself when it names a `Host` of
/// ~/.ssh/config that signs in as `user`, or else by a `Host` to add there, so no task needs the
/// address and the user typed out.
fn reaching(host: &str, resolved: &Resolved, user: &str) -> String {
    if !host.contains('@') && host != resolved.hostname && resolved.user == user {
        return format!(
            "The other tasks reach the server as {host}, such as `just deploy {host}`."
        );
    }
    if resolved.hostname.is_empty() {
        return format!(
            "The other tasks sign in as {user}, such as `just deploy {user}@<address>`."
        );
    }
    let port = if resolved.port.is_empty() || resolved.port == "22" {
        String::new()
    } else {
        format!("\n    Port {}", resolved.port)
    };
    format!(
        "The other tasks sign in as {user}. To name the server in them, as in `just deploy \
         production`, add these lines to ~/.ssh/config, with a name of your choice in place of \
         production:\n\nHost production\n    HostName {}\n    User {user}{port}\n",
        resolved.hostname
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(hostname: &str, port: &str, user: &str) -> Resolved {
        Resolved {
            hostname: hostname.to_owned(),
            port: port.to_owned(),
            user: user.to_owned(),
        }
    }

    #[test]
    fn ops_22_the_setup_ends_with_the_ssh_host_that_spares_the_other_tasks_the_address() {
        let as_root = reaching(
            "root@203.0.113.7",
            &resolved("203.0.113.7", "22", "root"),
            "deploy",
        );
        let on_a_port = reaching(
            "admin@203.0.113.7",
            &resolved("203.0.113.7", "2222", "admin"),
            "admin",
        );
        let named = reaching(
            "production",
            &resolved("203.0.113.7", "22", "deploy"),
            "deploy",
        );

        assert!(
            as_root.ends_with("Host production\n    HostName 203.0.113.7\n    User deploy\n"),
            "{as_root}"
        );
        assert!(
            on_a_port.ends_with("    User admin\n    Port 2222\n"),
            "{on_a_port}"
        );
        assert_eq!(
            named,
            "The other tasks reach the server as production, such as `just deploy production`."
        );
    }
}
