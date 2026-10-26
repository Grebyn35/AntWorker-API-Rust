//! Docker and Compose, run as an operator runs them.

use std::{
    io::{BufRead, BufReader},
    net::SocketAddr,
    process::{Command, Output, Stdio},
};

use serde_json::Value;

/// Runs `docker` with `args`, and returns what it wrote on stdout, trimmed. Panics with what it
/// wrote on stderr if it fails.
pub fn docker(args: &[&str]) -> String {
    let output = Command::new("docker")
        .args(args)
        .output()
        .expect("run docker");
    assert!(
        output.status.success(),
        "docker {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("docker's output")
        .trim()
        .to_owned()
}

/// Runs `docker compose` with `args`, in the Compose project of the repository, which Compose
/// finds from the crate's directory.
pub fn compose(args: &[&str]) -> String {
    docker(&[&["compose"], args].concat())
}

/// The network of the Postgres that Compose runs, on which a container reaches it as
/// `postgres`.
pub fn postgres_network() -> String {
    let id = compose(&["ps", "--quiet", "postgres"]);
    assert!(
        !id.is_empty(),
        "Postgres isn't running; run `just services`"
    );
    let networks: Value = serde_json::from_str(&docker(&[
        "inspect",
        "--format",
        "{{json .NetworkSettings.Networks}}",
        &id,
    ]))
    .expect("the container's networks");
    networks
        .as_object()
        .and_then(|networks| networks.keys().next())
        .expect("a network")
        .clone()
}

/// A running container. Dropping it removes the container.
pub struct Container {
    id: String,
}

impl Container {
    /// Starts a container with `args`, the arguments of `docker run` after `--detach`.
    pub fn start(args: &[&str]) -> Self {
        let id = docker(&[&["run", "--detach"], args].concat());
        Self { id }
    }

    /// The container's ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The value of `format`, a Go template, for the container, such as `{{.Config.User}}`.
    pub fn inspect(&self, format: &str) -> String {
        docker(&["inspect", "--format", format, &self.id])
    }

    /// Waits for Docker's first verdict on the container: `healthy` or `unhealthy`, from its
    /// health check, or `exited`. Docker reaches one once the health check's start period and
    /// retries run out, so the wait ends without a timeout of its own.
    pub fn health(&self) -> String {
        let created = self.inspect("{{.Created}}");
        let mut events = Command::new("docker")
            .args([
                "events",
                "--since",
                &created,
                "--filter",
                &format!("container={}", self.id),
                "--filter",
                "event=health_status",
                "--filter",
                "event=die",
                "--format",
                "{{.Status}}",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .expect("follow the container's events");
        let stdout = events.stdout.take().expect("the events");
        let verdict = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .find_map(|status| match status.trim() {
                "health_status: healthy" => Some("healthy"),
                "health_status: unhealthy" => Some("unhealthy"),
                "die" => Some("exited"),
                _ => None,
            })
            .unwrap_or("no verdict");
        events
            .kill()
            .and_then(|()| events.wait())
            .expect("stop following the events");
        verdict.to_owned()
    }

    /// The address on this machine at which the container's `port` is published.
    pub fn address(&self, port: u16) -> SocketAddr {
        docker(&["port", &self.id, &format!("{port}/tcp")])
            .lines()
            .next()
            .expect("a published port")
            .parse()
            .expect("an address")
    }

    /// What the container has written, on stdout and stderr.
    pub fn logs(&self) -> String {
        let output = Command::new("docker")
            .args(["logs", &self.id])
            .output()
            .expect("read the container's logs");
        lossy(&output)
    }
}

impl Drop for Container {
    /// Removes the container and its anonymous volumes. A removal that fails fails the test with
    /// what Docker said, and what it holds about the container and its daemon then.
    fn drop(&mut self) {
        let removed = Command::new("docker")
            .args(["rm", "--force", "--volumes", &self.id])
            .output();
        // A test that panicked reports its own failure, which a second panic would abort.
        if std::thread::panicking() {
            return;
        }
        let removed = removed.expect("run docker rm");
        let shell = |command: String| {
            Command::new("sh")
                .args(["-c", &command])
                .output()
                .map_or_else(|error| error.to_string(), |output| lossy(&output))
        };
        let id = &self.id;
        assert!(
            removed.status.success(),
            "remove the container {id}: {}\n\
             its state: {}\n\
             its mounts: {}\n\
             docker ps: {}\n\
             the daemon's log: {}",
            lossy(&removed),
            shell(format!(
                "docker inspect --format '{{{{json .State}}}}' {id}"
            )),
            shell(format!(
                "docker inspect --format '{{{{json .Mounts}}}}' {id}"
            )),
            shell(format!("docker ps --all --filter id={id}")),
            shell(
                "sudo journalctl --unit docker --since '-3 min' --no-pager 2>&1 | tail -n 40"
                    .to_owned()
            ),
        );
    }
}

/// Everything a process wrote, stdout then stderr, as text.
pub fn lossy(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
