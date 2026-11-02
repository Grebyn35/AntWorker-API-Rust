//! `load-test streams`, which `just load-test streams` runs: it measures a replica's hub (OPS-18,
//! the platform spec's open question on the hub). One replica of the image under test, with the
//! reference deployment's limits on CPU and memory, holds 1,000 open streams, 10 for each of 100
//! users, and a second replica, without limits, takes the requests that publish, as the other
//! replicas of a deployment do. The task reports the memory each open stream holds, then k6
//! publishes events at constant rates, raising the rate until a step fails, and narrowing the
//! gap, as the search for a replica's capacity does.
//!
//! An event is `invitation.received`, which inviting a stream's user again publishes to that user's
//! 10 streams (INVITATIONS-003, INVITATIONS-008), the one event of the template that changes no
//! one's access, so no stream closes. A step passes when no request failed, k6 dropped none, every
//! event reached each of its user's streams once, the streams of a user received the same events
//! in the same order, no stream closed, and the 99th percentile of the lag of the events that the
//! measured replica's hub delivered, which k6 reads from Prometheus, is within the latency
//! objective.

mod client;

use std::{
    collections::HashSet,
    path::Path,
    process::ExitCode,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use serde_json::{Value, json};

use super::{PORT, Replica, docker, fail, healthy, next_rate, report};
use client::Stream;

/// The users whose streams the measured replica holds.
const USERS: usize = 100;

/// The streams each user holds, the most one replica allows them (REALTIME-006).
const STREAMS_PER_USER: usize = 10;

/// The rate the search starts at, in events a second.
const FIRST_RATE: u64 = 50;

/// What a stream's thread tells the task.
enum News {
    /// The stream sent `stream.opened`.
    Opened,
    /// The stream sent `stream.closed`, or ended, or failed.
    Ended(usize, String),
}

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
    /// Whether the hub delivered the step's events, all of them, within the latency objective.
    fn passed(&self) -> bool {
        self.failed == 0
            && self.dropped == 0
            && self.delivered == self.requests
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
    let outcome = match start(root, replica, None) {
        Ok(publisher) => {
            let outcome = measure(root, &hub, &publisher, (cpus, memory), objective_ms);
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
    let mut args = vec![
        "run",
        "--detach",
        "--network",
        replica.network,
        "--read-only",
    ];
    if let Some((cpus, memory)) = limits {
        args.extend(["--cpus", cpus, "--memory", memory]);
    }
    args.extend([
        "--publish",
        &publish,
        "--env",
        &listen,
        "--env",
        "OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4318",
        // One inviter invites at the step's rate (INVITATIONS-009).
        "--env",
        "INVITATION_LIMIT=100000",
        "--env",
        "INVITATION_WINDOW_SECONDS=1",
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

/// The measurement, once both replicas run.
fn measure(
    root: &Path,
    hub: &str,
    publisher: &str,
    (cpus, memory): (&str, &str),
    objective_ms: f64,
) -> ExitCode {
    let (hub_address, publisher_inside, users) = match prepare(root, hub, publisher) {
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
    let Some(capacity) = steps
        .iter()
        .filter(|step| step.passed())
        .max_by_key(|step| step.rate)
    else {
        return fail("the hub delivered no rate the search tried");
    };
    report(&format!(
        "Hub capacity: {} events a second, each to the 10 streams of its user, {} frames a second to {} open streams, on one replica with {cpus} CPU and {memory} of memory, with none lost and the 99th percentile of their lag within {} ms. Each open stream holds about {per_stream:.1} KiB.",
        capacity.rate,
        capacity.rate * STREAMS_PER_USER as u64,
        USERS * STREAMS_PER_USER,
        capacity.objective_ms
    ));
    ExitCode::SUCCESS
}

/// Waits for both replicas to be ready, and returns the address on the host that reaches the
/// measured one, the address of the publishing one inside Compose's network, which k6 reaches,
/// and the users whose streams the measured one will hold.
fn prepare(
    root: &Path,
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
    // it, so a run needs no empty database.
    let run = hub.get(..12).unwrap_or(hub);
    let users = arrange(root, &publisher_address, run)?;
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

/// The users whose streams the hub holds, each with an access token, and the data k6 publishes
/// with: an inviter, who owns a workspace and has invited each user once. Their addresses hold
/// `run`.
fn arrange(root: &Path, publisher: &str, run: &str) -> Result<Vec<String>, String> {
    let register = |email: &str| -> Result<String, String> {
        let body =
            json!({ "email": email, "password": "plum-tractor-41-quiet", "name": "Load Test" });
        match client::request(publisher, "POST", "/v1/users", None, Some(&body))? {
            (201, answer) => answer["access_token"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("registering {email} answered no access token")),
            (status, answer) => Err(format!("registering {email} answered {status}: {answer}")),
        }
    };
    let inviter = register(&format!("load-{run}-inviter@example.com"))?;
    let workspace = match client::request(
        publisher,
        "POST",
        "/v1/workspaces",
        Some(&inviter),
        Some(&json!({ "name": "Load Test" })),
    )? {
        (201, answer) => answer["id"].as_str().unwrap_or_default().to_owned(),
        (status, answer) => {
            return Err(format!(
                "creating the workspace answered {status}: {answer}"
            ));
        }
    };
    let mut emails = Vec::new();
    let mut tokens = Vec::new();
    for n in 0..USERS {
        let email = format!("load-{run}-{n}@example.com");
        tokens.push(register(&email)?);
        let path = format!("/v1/workspaces/{workspace}/invitations");
        let body = json!({ "email": email, "role": "member" });
        match client::request(publisher, "POST", &path, Some(&inviter), Some(&body))? {
            (201, _) => {}
            (status, answer) => {
                return Err(format!("inviting {email} answered {status}: {answer}"));
            }
        }
        emails.push(email);
    }
    let data = json!({ "token": inviter, "workspace": workspace, "invitees": emails });
    let directory = root.join("target/load");
    write(&directory, &data).map_err(|error| format!("can't write the data k6 reads: {error}"))?;
    Ok(tokens)
}

#[expect(
    clippy::disallowed_methods,
    reason = "the task runs as a synchronous program, with no async runtime to stall"
)]
fn write(directory: &Path, data: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(directory)?;
    std::fs::write(directory.join("streams.json"), data.to_string())
}

/// The events each stream received, by their IDs, in order.
type Received = Arc<Mutex<Vec<Vec<String>>>>;

/// The open streams: what each received, and what their threads tell.
struct Streams {
    received: Received,
    news: mpsc::Receiver<News>,
}

/// Opens 10 streams for each of `tokens`' users on the hub at `address`, each read on a thread of
/// its own, and returns once every one has sent `stream.opened`.
fn open_streams(address: &str, tokens: &[String]) -> Result<Streams, String> {
    let count = tokens.len() * STREAMS_PER_USER;
    let received: Received = Arc::new(Mutex::new(vec![Vec::new(); count]));
    let (tell, news) = mpsc::channel();
    for index in 0..count {
        let token = tokens[index / STREAMS_PER_USER].clone();
        let address = address.to_owned();
        let received = received.clone();
        let tell = tell.clone();
        thread::spawn(move || {
            let ended = read(&address, &token, index, &received, &tell);
            // The task may have finished, and the receiver gone with it.
            tell.send(News::Ended(index, ended)).unwrap_or_default();
        });
    }
    // A stream that the replica doesn't answer ends at its socket's timeout, so each one tells.
    for _ in 0..count {
        match news.recv() {
            Ok(News::Opened) => {}
            Ok(News::Ended(index, why)) => {
                return Err(format!("stream {index} ended before it opened: {why}"));
            }
            Err(error) => return Err(format!("the streams didn't all open: {error}")),
        }
    }
    Ok(Streams { received, news })
}

/// Reads the stream `index` of `token`'s caller until it ends, and says why it ended.
fn read(
    address: &str,
    token: &str,
    index: usize,
    received: &Received,
    tell: &mpsc::Sender<News>,
) -> String {
    let mut stream = match Stream::open(address, token) {
        Ok(stream) => stream,
        Err(error) => return error,
    };
    loop {
        match stream.next() {
            Ok(Some(frame)) => match frame.event.as_deref() {
                Some("stream.opened") => {
                    if tell.send(News::Opened).is_err() {
                        return "the task stopped".to_owned();
                    }
                }
                Some("stream.closed") => return "stream.closed".to_owned(),
                _ => {
                    if let (Some(id), Ok(mut received)) = (frame.id, received.lock()) {
                        received[index].push(id);
                    }
                }
            },
            Ok(None) => return "the response ended".to_owned(),
            Err(error) => return error,
        }
    }
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

/// The events the streams received since `before`, counted once for each user, and whether each
/// user's streams received the same ones, in the same order, none twice.
fn delivered(received: &[Vec<String>], before: &[usize]) -> (u64, bool) {
    let mut delivered = 0;
    let mut consistent = true;
    for (user, streams) in received.chunks(STREAMS_PER_USER).enumerate() {
        let new: Vec<&[String]> = streams
            .iter()
            .enumerate()
            .map(|(n, events)| &events[before[user * STREAMS_PER_USER + n].min(events.len())..])
            .collect();
        let first = new.first().copied().unwrap_or_default();
        let unique: HashSet<&String> = first.iter().collect();
        consistent &= unique.len() == first.len() && new.iter().all(|events| *events == first);
        delivered += first.len() as u64;
    }
    (delivered, consistent)
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

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn ops_18_a_user_s_streams_must_receive_the_same_events_once_each_in_order() {
        let same = vec![ids(&["a", "b"]); STREAMS_PER_USER];
        let mut missing = same.clone();
        missing[3] = ids(&["a"]);
        let mut twice = vec![ids(&["a", "a"]); STREAMS_PER_USER];
        twice[0] = ids(&["a", "a"]);
        let mut reordered = same.clone();
        reordered[9] = ids(&["b", "a"]);
        let before = vec![0; STREAMS_PER_USER];

        assert_eq!(delivered(&same, &before), (2, true));
        assert!(!delivered(&missing, &before).1);
        assert!(!delivered(&twice, &before).1);
        assert!(!delivered(&reordered, &before).1);
        assert_eq!(delivered(&same, &[1; STREAMS_PER_USER]), (1, true));
    }

    #[test]
    fn ops_18_a_hub_step_passes_only_with_every_event_delivered_in_time() {
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
    fn ops_18_docker_s_memory_is_read_in_bytes() {
        assert_eq!(bytes("12.5MiB"), 13_107_200.0);
        assert_eq!(bytes("512KiB"), 524_288.0);
        assert_eq!(bytes("1.5GiB"), 1_610_612_736.0);
    }
}
