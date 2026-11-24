//! `load-test streams`, which `just load-test streams` runs: it measures a replica's hub (OPS-16,
//! the platform spec's "Events and the real-time hub", under Capacity). One replica of the image under test, with the
//! reference deployment's limits on CPU and memory, holds 1,000 open streams, 10 for each of 100
//! users, and a second replica, without limits, takes the requests that publish, as the other
//! replicas of a deployment do. The task reports the memory each open stream holds, then k6
//! publishes events at constant rates, raising the rate until a step fails, and narrowing the
//! gap, as the search for a replica's capacity does.
//!
//! Each user owns a workspace of their own, of which they are the only member, and an event is
//! `workspace.updated`, which renaming it publishes to its members' streams (WORKSPACES-008), so
//! to that user's 10: an event of the core, which every fork keeps, and one that changes no one's
//! access, so no stream closes. A step passes when no request failed, k6 dropped none, every
//! event reached each of its user's streams once, the streams of a user received the same events
//! in the same order, no stream closed, and the 99th percentile of the lag of the events that the
//! measured replica's hub delivered, which k6 reads from Prometheus, is within the latency
//! objective.

mod arrange;
mod client;
mod readers;

use std::{path::Path, process::ExitCode};

use serde_json::Value;

use super::{LABEL, PORT, docker, fail, healthy, next_rate, report};
use readers::{Streams, delivered, open_streams};

/// What a replica of the image under test runs with: the image, the network of Compose's
/// Postgres, and its settings.
pub(super) struct Replica<'a> {
    pub(super) image: &'a str,
    pub(super) network: &'a str,
    pub(super) settings: &'a [String],
}

/// The users whose streams the measured replica holds.
const USERS: usize = 100;

/// The streams each user holds, the most one replica allows them (REALTIME-006).
const STREAMS_PER_USER: usize = 10;

/// The rate the search starts at, in events a second.
const FIRST_RATE: u64 = 50;

/// What k6 and the streams reported for one step.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct HubStep {
    rate: u64,
    requests: u64,
    failed: u64,
    dropped: u64,
    p99_ms: f64,
    /// The 99th percentile of the measured hub's lag, if Prometheus had it.
    lag_p99_ms: Option<f64>,
    /// The events the streams received, counted once for each user.
    delivered: u64,
    /// Whether the 10 streams of each user received the same events, in the same order, none
    /// twice.
    consistent: bool,
    /// The streams that closed or failed during the step.
    ended: usize,
    objective_ms: f64,
}

impl HubStep {
    /// Whether the step passed: every event published, and the hub kept up with them.
    fn passed(&self) -> bool {
        self.published() && self.kept_up()
    }

    /// Whether the publishing replica published every event the step asked for.
    fn published(&self) -> bool {
        self.failed == 0 && self.dropped == 0
    }

    /// Whether the hub delivered every event that was published, to each of its user's streams,
    /// within the latency objective, closing none.
    fn kept_up(&self) -> bool {
        self.delivered == self.requests.saturating_sub(self.failed)
            && self.consistent
            && self.ended == 0
            && self.lag_p99_ms.is_some_and(|lag| lag <= self.objective_ms)
    }
}

/// Measures the hub of a replica of `replica` with the limits `(cpus, memory)`, against the
/// latency objective `objective_ms`.
pub(super) fn run(
    root: &Path,
    replica: &Replica<'_>,
    (cpus, memory): (&str, &str),
    objective_ms: f64,
) -> ExitCode {
    let hub = match start(root, replica, Some((cpus, memory))) {
        Ok(id) => id,
        Err(error) => return fail(&format!("the measured replica didn't start: {error}")),
    };
    // The publishing replica's pool and concurrency limit are wide, so that its own limits end
    // the search as late as they can (OPS-07).
    let outcome = match start(root, replica, None) {
        Ok(publisher) => {
            let replicas = (hub.as_str(), publisher.as_str(), replica.settings);
            let outcome = measure(root, replicas, (cpus, memory), objective_ms);
            remove(root, &publisher);
            outcome
        }
        Err(error) => fail(&format!("the publishing replica didn't start: {error}")),
    };
    remove(root, &hub);
    outcome
}

/// Starts a replica of `replica`, with `limits` when given, whose port the host reaches.
fn start(
    root: &Path,
    replica: &Replica<'_>,
    limits: Option<(&str, &str)>,
) -> Result<String, String> {
    let listen = format!("LISTEN_ADDR=0.0.0.0:{PORT}");
    let publish = format!("127.0.0.1::{PORT}");
    let label = format!("{LABEL}=true");
    let mut args = vec![
        "run",
        "--detach",
        "--label",
        &label,
        "--network",
        replica.network,
        "--read-only",
    ];
    match limits {
        Some((cpus, memory)) => args.extend(["--cpus", cpus, "--memory", memory]),
        None => args.extend([
            "--env",
            "DATABASE_MAX_CONNECTIONS=40",
            "--env",
            "MAX_CONCURRENT_REQUESTS=10000",
        ]),
    }
    args.extend([
        "--publish",
        &publish,
        "--env",
        &listen,
        "--env",
        "OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4318",
    ]);
    for setting in replica.settings {
        args.extend(["--env", setting]);
    }
    args.extend([replica.image, "serve"]);
    docker(root, &args)
}

fn remove(root: &Path, id: &str) {
    if let Err(error) = docker(root, &["rm", "--force", id]) {
        report(&format!("couldn't remove the replica {id}: {error}"));
    }
}

/// The measurement, once both replicas run: the measured one, the publishing one, and the
/// settings that both run with.
fn measure(
    root: &Path,
    (hub, publisher, settings): (&str, &str, &[String]),
    (cpus, memory): (&str, &str),
    objective_ms: f64,
) -> ExitCode {
    let (hub_address, publisher_inside, users) = match prepare(root, settings, hub, publisher) {
        Ok(prepared) => prepared,
        Err(error) => return fail(&error),
    };
    let before = memory_of(root, hub);
    let streams = match open_streams(&hub_address, &users) {
        Ok(streams) => streams,
        Err(error) => return fail(&error),
    };
    let after = memory_of(root, hub);
    let per_stream = (after - before) / (USERS * STREAMS_PER_USER) as f64 / 1024.0;
    report(&format!(
        "{} streams open: the replica's memory went from {:.1} MiB to {:.1} MiB, about {per_stream:.1} KiB a stream",
        USERS * STREAMS_PER_USER,
        before / 1_048_576.0,
        after / 1_048_576.0
    ));
    let mut steps: Vec<HubStep> = Vec::new();
    loop {
        let outcomes: Vec<(u64, bool)> = steps
            .iter()
            .map(|step| (step.rate, step.passed()))
            .collect();
        let Some(rate) = next_rate(FIRST_RATE, &outcomes) else {
            break;
        };
        let step = match step(root, &publisher_inside, rate, &streams, objective_ms) {
            Ok(step) => step,
            Err(error) => return fail(&error),
        };
        let state = docker(
            root,
            &[
                "inspect",
                "--format",
                "{{.State.Status}}, killed for its memory: {{.State.OOMKilled}}",
                hub,
            ],
        )
        .unwrap_or_default();
        if !state.starts_with("running") {
            return fail(&format!(
                "the measured replica stopped during the step at {rate} events a second: {state}"
            ));
        }
        report_step(&step, memory_of(root, hub));
        steps.push(step);
    }
    let Some(lines) = closing(&steps, per_stream, (cpus, memory)) else {
        return fail("the hub delivered no rate the search tried");
    };
    for line in lines {
        report(&line);
    }
    ExitCode::SUCCESS
}

/// The lines that end the report of `steps`, on a replica with the limits `(cpus, memory)`
/// whose open streams each held `per_stream` KiB, the last of them the one that starts
/// `Hub capacity: ` (OPS-16), or `None` when no step passed.
fn closing(
    steps: &[HubStep],
    per_stream: f64,
    (cpus, memory): (&str, &str),
) -> Option<Vec<String>> {
    let capacity = steps
        .iter()
        .filter(|step| step.passed())
        .max_by_key(|step| step.rate)?;
    let mut lines = Vec::new();
    // When the hub kept up at every step, the publishing replica's limit ended the search, and
    // the hub's capacity is higher than the highest rate it was given.
    let bound = if steps.iter().all(HubStep::kept_up) {
        lines.push(
            "The hub kept up at every step: the steps that failed failed to publish.".to_owned(),
        );
        "at least "
    } else {
        ""
    };
    lines.push(format!(
        "Hub capacity: {bound}{} events a second, each to the 10 streams of one user, {} frames a second to {} open streams, on one replica with {cpus} CPU and {memory} of memory, with none lost and the 99th percentile of their lag within {} ms. Each open stream holds about {per_stream:.1} KiB.",
        capacity.rate,
        capacity.rate * STREAMS_PER_USER as u64,
        USERS * STREAMS_PER_USER,
        capacity.objective_ms
    ));
    Some(lines)
}

/// Waits for both replicas to be ready, and returns the address on the host that reaches the
/// measured one, the address of the publishing one inside Compose's network, which k6 reaches,
/// and the users whose streams the measured one will hold, arranged in the database that the
/// replicas' `settings` name.
fn prepare(
    root: &Path,
    settings: &[String],
    hub: &str,
    publisher: &str,
) -> Result<(String, String, Vec<String>), String> {
    for id in [hub, publisher] {
        healthy(root, id)?;
    }
    let hub_address = host_address(root, hub)?;
    let publisher_address = host_address(root, publisher)?;
    let inside = docker(
        root,
        &[
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            publisher,
        ],
    )?;
    // The measured replica's container is new on each run, and so are the addresses named after
    // it.
    let run = hub.get(..12).unwrap_or(hub);
    let users = arrange::arrange(root, settings, &publisher_address, run)?;
    Ok((hub_address, inside, users))
}

/// Reports `step`, after which the measured replica used `memory` bytes.
fn report_step(step: &HubStep, memory: f64) {
    report(&format!(
        "{} events a second, {} frames a second: {} published, {} failed, {} dropped, request p99 {:.1} ms, {} delivered{}, {} streams ended, lag p99 {}, memory {:.1} MiB: {}",
        step.rate,
        step.rate * STREAMS_PER_USER as u64,
        step.requests,
        step.failed,
        step.dropped,
        step.p99_ms,
        step.delivered,
        if step.consistent {
            ""
        } else {
            ", not the same on each of a user's streams"
        },
        step.ended,
        step.lag_p99_ms
            .map_or_else(|| "unknown".to_owned(), |lag| format!("{lag:.1} ms")),
        memory / 1_048_576.0,
        if step.passed() { "passed" } else { "failed" }
    ));
}

/// The address on the host that reaches the replica `id`'s port.
fn host_address(root: &Path, id: &str) -> Result<String, String> {
    let ports = docker(root, &["port", id, &format!("{PORT}/tcp")])?;
    ports
        .lines()
        .find(|line| line.starts_with("127.0.0.1:"))
        .map(str::to_owned)
        .ok_or_else(|| format!("the replica {id} publishes no port on 127.0.0.1: {ports}"))
}

/// Runs one step at `rate` through the publishing replica at `publisher`, and reads what the
/// streams received during it.
fn step(
    root: &Path,
    publisher: &str,
    rate: u64,
    streams: &Streams,
    objective_ms: f64,
) -> Result<HubStep, String> {
    let before: Vec<usize> = streams
        .received
        .lock()
        .map_err(|_| "a stream's thread panicked")?
        .iter()
        .map(Vec::len)
        .collect();
    let output = docker(
        root,
        &[
            "compose",
            "run",
            "--rm",
            "--no-deps",
            "--no-TTY",
            "--volume",
            "./crates/app/tests/load:/load:ro",
            "--volume",
            "./target/load:/data:ro",
            "--env",
            &format!("TARGET=http://{publisher}:{PORT}"),
            "--env",
            &format!("RATE={rate}"),
            "--env",
            "DATA=/data/streams.json",
            "--env",
            "PROMETHEUS=http://prometheus:9090",
            "k6",
            "run",
            "--quiet",
            "/load/streams.js",
        ],
    )?;
    let outcome: Value = output
        .lines()
        .rev()
        .find_map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .filter(|value| value["rate"].is_u64())
        })
        .ok_or_else(|| format!("k6 reported no outcome: {output}"))?;
    let ended = streams.news.try_iter().count();
    let received = streams
        .received
        .lock()
        .map_err(|_| "a stream's thread panicked")?;
    let (delivered, consistent) = delivered(&received, &before);
    let count = |name: &str| outcome[name].as_u64().unwrap_or_default();
    Ok(HubStep {
        rate,
        requests: count("requests"),
        failed: count("failed"),
        dropped: count("dropped"),
        p99_ms: outcome["p99_ms"].as_f64().unwrap_or_default(),
        lag_p99_ms: outcome["lag_p99_ms"].as_f64(),
        delivered,
        consistent,
        ended,
        objective_ms,
    })
}

/// The memory the container `id` uses, in bytes, as Docker reports it.
fn memory_of(root: &Path, id: &str) -> f64 {
    let used = docker(
        root,
        &["stats", "--no-stream", "--format", "{{.MemUsage}}", id],
    )
    .unwrap_or_default();
    bytes(used.split('/').next().unwrap_or_default().trim())
}

/// The bytes that Docker writes as `12.5MiB`.
fn bytes(text: &str) -> f64 {
    let split = text
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().unwrap_or_default();
    let scale = match unit {
        "KiB" | "kB" => 1024.0,
        "MiB" | "MB" => 1_048_576.0,
        "GiB" | "GB" => 1_073_741_824.0,
        _ => 1.0,
    };
    number * scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_16_a_hub_step_passes_only_with_every_event_delivered_in_time() {
        let passing = HubStep {
            rate: 100,
            requests: 3000,
            failed: 0,
            dropped: 0,
            p99_ms: 20.0,
            lag_p99_ms: Some(40.0),
            delivered: 3000,
            consistent: true,
            ended: 0,
            objective_ms: 500.0,
        };

        assert!(passing.passed());
        let shed = HubStep {
            failed: 100,
            delivered: 2900,
            ..passing.clone()
        };
        assert!(!shed.passed());
        assert!(shed.kept_up());
        for failing in [
            HubStep {
                delivered: 2999,
                ..passing.clone()
            },
            HubStep {
                consistent: false,
                ..passing.clone()
            },
            HubStep {
                ended: 1,
                ..passing.clone()
            },
            HubStep {
                lag_p99_ms: None,
                ..passing.clone()
            },
            HubStep {
                lag_p99_ms: Some(501.0),
                ..passing.clone()
            },
            HubStep {
                failed: 1,
                ..passing.clone()
            },
        ] {
            assert!(!failing.passed(), "{failing:?}");
        }
    }

    #[test]
    fn ops_16_the_streams_task_ends_with_the_line_of_the_hub_s_capacity() {
        let passing = HubStep {
            rate: 100,
            requests: 3000,
            failed: 0,
            dropped: 0,
            p99_ms: 20.0,
            lag_p99_ms: Some(40.0),
            delivered: 3000,
            consistent: true,
            ended: 0,
            objective_ms: 500.0,
        };
        // A step whose publishing replica failed requests, while the hub kept up.
        let unpublished = HubStep {
            rate: 200,
            failed: 100,
            delivered: 2900,
            ..passing.clone()
        };
        let late = HubStep {
            lag_p99_ms: Some(501.0),
            ..unpublished.clone()
        };

        let kept_up = closing(&[passing.clone(), unpublished], 9.5, ("1", "512m"));
        let fell_behind = closing(&[passing.clone(), late], 9.5, ("1", "512m"));
        let none_passed = closing(&[], 9.5, ("1", "512m"));

        let last = |lines: &Option<Vec<String>>| lines.as_ref()?.last().cloned();
        assert!(
            last(&kept_up).is_some_and(|line| line.starts_with("Hub capacity: at least 100 ")),
            "{kept_up:?}"
        );
        assert_eq!(kept_up.as_ref().map(Vec::len), Some(2), "{kept_up:?}");
        assert!(
            last(&fell_behind).is_some_and(|line| line.starts_with("Hub capacity: 100 ")),
            "{fell_behind:?}"
        );
        assert_eq!(
            fell_behind.as_ref().map(Vec::len),
            Some(1),
            "{fell_behind:?}"
        );
        assert_eq!(none_passed, None);
    }

    #[test]
    fn ops_16_docker_s_memory_is_read_in_bytes() {
        assert_eq!(bytes("12.5MiB"), 13_107_200.0);
        assert_eq!(bytes("512KiB"), 524_288.0);
        assert_eq!(bytes("1.5GiB"), 1_610_612_736.0);
    }
}
