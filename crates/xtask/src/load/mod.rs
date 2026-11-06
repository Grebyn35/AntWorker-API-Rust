//! `load-test`, which `just load-test` runs: it measures one replica's capacity (OPS-16). It runs
//! one replica of the API's image under test, with the reference deployment's limits on CPU and
//! memory for a replica, against Compose's Postgres, with its telemetry exported to Compose's
//! collector. Then k6 sends it requests at constant rates, raising the rate until a step fails,
//! and narrowing the gap. A step passes when no request failed, k6 dropped none for want of a
//! client, and the 99th percentile of their durations is within the latency objective, which it
//! reads from the collector's tail sampling, as `check-telemetry-config` does. The highest rate
//! that passed is the capacity.
//!
//! Each step also compares the requests the replica served with the spans the collector
//! accepted, so a span that the API's queue dropped under load shows as a difference (the
//! platform spec's Telemetry export, under Batching).
//!
//! The search is a pure function, so tests cover it without Docker.
// part: invitations
//!
//! `streams` measures a replica's hub instead (OPS-16): see its module.
// end part: invitations

use std::{
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use serde_json::Value;

use crate::{repo::Repo, telemetry_config};
use replica::{LABEL, healthy, hold_the_machine, image, limits, prepare, start_the_stack};

mod replica;
// part: invitations
mod streams;
// end part: invitations

/// The rate the search for a replica's capacity starts at, in requests a second.
const FIRST_RATE: u64 = 200;
/// The port `serve` binds in the replica's container.
const PORT: u16 = 8080;

/// What k6 reported for one step.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub rate: u64,
    pub requests: u64,
    pub failed: u64,
    pub dropped: u64,
    pub p99_ms: f64,
    pub spans: u64,
    /// The latency objective the step was measured against, in milliseconds.
    pub objective_ms: f64,
}

impl Step {
    /// The step's rate, and whether the replica served it.
    fn outcome(&self) -> (u64, bool) {
        (self.rate, self.passed())
    }

    /// Whether the replica served the step's rate.
    pub fn passed(&self) -> bool {
        self.failed == 0 && self.dropped == 0 && self.p99_ms <= self.objective_ms
    }

    fn parse(line: &str, objective_ms: f64) -> Option<Self> {
        let value: Value = serde_json::from_str(line).ok()?;
        let count = |name: &str| value[name].as_u64().unwrap_or_default();
        Some(Self {
            rate: value["rate"].as_u64()?,
            requests: count("requests"),
            failed: count("failed"),
            dropped: count("dropped"),
            p99_ms: value["p99_ms"].as_f64().unwrap_or_default(),
            spans: count("spans"),
            objective_ms,
        })
    }
}

/// The rate to try after `steps`, each a rate and whether it passed, or none once the highest
/// passing rate and the lowest failing one are within a tenth of each other: the rate starts at
/// `first`, doubles until a step fails, then the search halves the gap.
pub fn next_rate(first: u64, steps: &[(u64, bool)]) -> Option<u64> {
    let passed = steps
        .iter()
        .filter(|(_, passed)| *passed)
        .map(|(rate, _)| *rate)
        .max();
    let failed = steps
        .iter()
        .filter(|(_, passed)| !passed)
        .map(|(rate, _)| *rate)
        .min();
    match (passed, failed) {
        (None, None) => Some(first),
        (Some(passed), None) => Some(passed * 2),
        (None, Some(failed)) if failed > 10 => Some(failed / 2),
        (None, Some(_)) => None,
        (Some(passed), Some(failed)) if failed - passed > (passed / 10).max(10) => {
            Some(passed + (failed - passed) / 2)
        }
        (Some(_), Some(_)) => None,
    }
}

/// Runs the load test against the API's image under test, once it holds the machine and has pushed
/// the image: of a replica's capacity on `route`.
// part: invitations
/// With `streams`, it measures the replica's hub instead.
// end part: invitations
pub fn run(route: &str) -> ExitCode {
    let path = path_of(route);
    // part: invitations
    let path = path.or((route == "streams").then_some(""));
    // end part: invitations
    let Some(path) = path else {
        let names = ["capabilities", "me"];
        // part: invitations
        let names = [names.as_slice(), &["streams"]].concat();
        // end part: invitations
        return fail(&format!("no route is named {route}: {}", names.join(", ")));
    };
    let root = root();
    let held = hold_the_machine(&root).and_then(|machine| start_the_stack(&root).map(|()| machine));
    let _machine = match held {
        Ok(machine) => machine,
        Err(error) => return fail(&error),
    };
    let image = match image(&root) {
        Ok(image) => image,
        Err(error) => return fail(&error),
    };
    let image = image.as_str();
    let (cpus, memory) = match limits(&root) {
        Ok(limits) => limits,
        Err(error) => return fail(&error),
    };
    let objective_ms = match Repo::load()
        .map_err(|error| error.to_string())
        .and_then(|repo| telemetry_config::latency_objective(&repo))
    {
        Ok(objective_ms) => objective_ms,
        Err(error) => return fail(&format!("can't read the latency objective: {error}")),
    };
    let (network, settings) = match prepare(&root, image) {
        Ok(prepared) => prepared,
        Err(error) => return fail(&error),
    };
    // part: invitations
    if route == "streams" {
        let replica = streams::Replica {
            image,
            network: &network,
            settings: &settings,
        };
        return streams::run(&root, &replica, (&cpus, &memory), objective_ms);
    }
    // end part: invitations
    let listen = format!("LISTEN_ADDR=0.0.0.0:{PORT}");
    let label = format!("{LABEL}=true");
    let mut args = vec![
        "run",
        "--detach",
        "--label",
        &label,
        "--network",
        &network,
        "--cpus",
        &cpus,
        "--memory",
        &memory,
        "--read-only",
        "--env",
        &listen,
        "--env",
        "OTEL_EXPORTER_OTLP_ENDPOINT=http://collector:4318",
    ];
    for setting in &settings {
        args.extend(["--env", setting]);
    }
    args.extend([image, "serve"]);
    let replica = match docker(&root, &args) {
        Ok(id) => id,
        Err(error) => return fail(&format!("the replica didn't start: {error}")),
    };
    let outcome = measure(
        &root,
        &replica,
        (route, path),
        (&cpus, &memory),
        objective_ms,
    );
    let removed = docker(&root, &["rm", "--force", &replica]);
    if let Err(error) = removed {
        report(&format!("couldn't remove the replica {replica}: {error}"));
    }
    outcome
}

/// The path of the route the load test can measure, by its name.
fn path_of(route: &str) -> Option<&'static str> {
    match route {
        "capabilities" => Some("GET /v1/capabilities"),
        "me" => Some("GET /v1/users/me"),
        _ => None,
    }
}

/// Measures the replica `replica`, which runs with the limits `(cpus, memory)`, on the route
/// `(route, path)`, against the latency objective `objective_ms`.
fn measure(
    root: &Path,
    replica: &str,
    (route, path): (&str, &str),
    (cpus, memory): (&str, &str),
    objective_ms: f64,
) -> ExitCode {
    if let Err(error) = healthy(root, replica) {
        return fail(&error);
    }
    let address = match docker(
        root,
        &[
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            replica,
        ],
    ) {
        Ok(address) => address,
        Err(error) => return fail(&error),
    };
    let mut steps: Vec<Step> = Vec::new();
    while let Some(rate) = next_rate(FIRST_RATE, &outcomes(&steps)) {
        let step = match step(root, &address, route, rate, objective_ms) {
            Ok(step) => step,
            Err(error) => return fail(&error),
        };
        // A replica that the step ended, such as one its memory limit killed, measures nothing
        // more.
        let state = docker(
            root,
            &[
                "inspect",
                "--format",
                "{{.State.Status}}, killed for its memory: {{.State.OOMKilled}}",
                replica,
            ],
        )
        .unwrap_or_default();
        if !state.starts_with("running") {
            return fail(&format!(
                "the replica stopped during the step at {rate} requests a second: {state}"
            ));
        }
        let used = docker(
            root,
            &["stats", "--no-stream", "--format", "{{.MemUsage}}", replica],
        )
        .unwrap_or_default();
        report(&format!(
            "{rate} requests a second: {} served, {} failed, {} dropped, p99 {:.1} ms, {} spans accepted, memory {used}: {}",
            step.requests,
            step.failed,
            step.dropped,
            step.p99_ms,
            step.spans,
            if step.passed() { "passed" } else { "failed" }
        ));
        steps.push(step);
    }
    let Some(capacity) = steps
        .iter()
        .filter(|step| step.passed())
        .max_by_key(|step| step.rate)
    else {
        return fail("the replica served no rate the search tried");
    };
    report(&format!(
        "Capacity: {} requests a second to {path} on one replica with {cpus} CPU and {memory} of memory, with the 99th percentile within {} ms and none failed. The collector accepted {} spans for its {} requests.",
        capacity.rate, capacity.objective_ms, capacity.spans, capacity.requests
    ));
    ExitCode::SUCCESS
}

/// Each step's rate, and whether it passed.
fn outcomes(steps: &[Step]) -> Vec<(u64, bool)> {
    steps.iter().map(Step::outcome).collect()
}

/// Runs one step at `rate` on `route` of the replica at `address`, which passes within the
/// latency objective `objective_ms`.
fn step(
    root: &Path,
    address: &str,
    route: &str,
    rate: u64,
    objective_ms: f64,
) -> Result<Step, String> {
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
            "--env",
            &format!("TARGET=http://{address}:{PORT}"),
            "--env",
            &format!("RATE={rate}"),
            "--env",
            &format!("ROUTE={route}"),
            "--env",
            "COLLECTOR=http://collector:8888",
            "k6",
            "run",
            "--quiet",
            "/load/capacity.js",
        ],
    )?;
    output
        .lines()
        .rev()
        .find_map(|line| Step::parse(line, objective_ms))
        .ok_or_else(|| format!("k6 reported no outcome: {output}"))
}

/// The ID of the container of Compose's service `service`.
fn compose_id(root: &Path, service: &str) -> String {
    docker(root, &["compose", "ps", "--quiet", service]).unwrap_or_default()
}

/// Runs docker with `args` in `root`, and returns what it wrote on stdout, trimmed.
fn docker(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("docker")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("can't run docker: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[expect(
    clippy::disallowed_methods,
    reason = "the task runs as a synchronous program, with no async runtime to stall"
)]
fn read(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

#[expect(clippy::print_stdout, reason = "the report is the task's output")]
fn report(message: &str) {
    println!("{message}");
}

#[expect(clippy::print_stderr, reason = "the failure is the task's output")]
fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(rate: u64, passed: bool) -> Step {
        Step {
            rate,
            requests: rate * 30,
            failed: u64::from(!passed),
            dropped: 0,
            p99_ms: 12.0,
            spans: rate * 30,
            objective_ms: 500.0,
        }
    }

    #[test]
    fn ops_16_the_rate_doubles_until_a_step_fails_then_the_gap_halves_to_a_tenth() {
        let mut steps = Vec::new();
        let mut tried = Vec::new();
        // A replica that serves up to 1,000 requests a second.
        while let Some(rate) = next_rate(FIRST_RATE, &outcomes(&steps)) {
            tried.push(rate);
            steps.push(step(rate, rate <= 1000));
        }

        assert_eq!(tried, [200, 400, 800, 1600, 1200, 1000, 1100]);
        let capacity = steps
            .iter()
            .filter(|step| step.passed())
            .map(|step| step.rate)
            .max();
        assert_eq!(capacity, Some(1000));
    }

    #[test]
    fn ops_16_a_step_fails_on_a_failed_or_dropped_request_or_a_slow_99th_percentile() {
        let passing = step(400, true);
        let slow = Step {
            p99_ms: 501.0,
            ..passing.clone()
        };
        let dropped = Step {
            dropped: 1,
            ..passing.clone()
        };
        let failed = Step {
            failed: 1,
            ..passing.clone()
        };

        assert!(passing.passed());
        assert!(!slow.passed());
        assert!(!dropped.passed());
        assert!(!failed.passed());
    }

    #[test]
    fn ops_16_k6_s_outcome_line_is_read() {
        let line =
            r#"{"rate":400,"requests":12000,"failed":0,"dropped":0,"p99_ms":8.25,"spans":12000}"#;

        assert_eq!(
            Step::parse(line, 500.0),
            Some(Step {
                rate: 400,
                requests: 12000,
                failed: 0,
                dropped: 0,
                p99_ms: 8.25,
                spans: 12000,
                objective_ms: 500.0,
            })
        );
    }
}
