//! Docker and Compose, run as an operator runs them.

use std::{
    io::{BufRead, BufReader},
    net::SocketAddr,
    process::{Command, Stdio},
};

use serde_json::Value;

use crate::machine;

/// Everything a process wrote, stdout then stderr, as text: the test support's `printed`, which
/// these tests take under a name of their own.
pub use crate::support::operator::printed as lossy;

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

/// The image of curl among the deployment's tools, as `deploy/tools/compose.yaml` names it, which
/// a test runs on a server or in the stack.
pub fn curl_image() -> String {
    compose(&[
        "--file",
        "../../deploy/tools/compose.yaml",
        "config",
        "--images",
        "curl",
    ])
}

/// Compose's network, on which a container reaches the development Postgres as `postgres`, and
/// the tests' Postgres as `postgres-tests`.
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
    /// Starts a container with `args`, the arguments of `docker run` after `--detach`, labelled
    /// as the tests' own, once the run holds the machine (machine.rs).
    pub fn start(args: &[&str]) -> Self {
        machine::claim();
        let label = format!("{}=true", machine::LABEL);
        let id = docker(&[&["run", "--detach", "--label", &label], args].concat());
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
    /// health check, or `exited`, or `no verdict` once [`Container::verdict_due`] passes without
    /// one, when Docker's events, which `--until` bounds, end.
    pub fn health(&self) -> String {
        let created = self.inspect("{{.Created}}");
        let due = self.verdict_due();
        let mut events = Command::new("docker")
            .args([
                "events",
                "--since",
                &created,
                "--until",
                &due,
                "--filter",
                &format!("container={}", self.id),
                "--filter",
                "event=health_status",
                "--filter",
                "event=die",
                "--format",
                "{{.Action}}",
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

    /// The moment, in whole seconds since 1970, at which the wait for Docker's verdict on the
    /// container gives up: the container's start, then its health check's start period, within
    /// which Docker counts no failure, then one more probe than the check's retries, each of an
    /// interval and a timeout. Each is the container's, as `.Config.Healthcheck` holds it, or
    /// else Docker's default: 30 seconds for an interval and a timeout, no start period, and 3
    /// retries.
    fn verdict_due(&self) -> String {
        let check: Value = serde_json::from_str(&self.inspect("{{json .Config.Healthcheck}}"))
            .expect("the health check");
        let given = |name: &str, default: u64| {
            check[name]
                .as_u64()
                .filter(|given| *given > 0)
                .unwrap_or(default)
        };
        let second = 1_000_000_000;
        let probe = given("Interval", 30 * second) + given("Timeout", 30 * second);
        let bound = given("StartPeriod", 0) + (given("Retries", 3) + 1) * probe;
        let started = time::OffsetDateTime::parse(
            &self.inspect("{{.State.StartedAt}}"),
            &time::format_description::well_known::Rfc3339,
        )
        .expect("the moment the container started");
        let due = started + std::time::Duration::from_nanos(bound);
        (due.unix_timestamp() + 1).to_string()
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
                "sudo -n journalctl --unit docker --since '-3 min' --no-pager 2>&1 | tail -n 40"
                    .to_owned()
            ),
        );
    }
}
