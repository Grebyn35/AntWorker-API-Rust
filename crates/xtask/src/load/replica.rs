//! The replica that `just load-test` measures: the image it runs, its limits and settings, the
//! database it is prepared on, and the wait until it is healthy.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
};

use serde_json::Value;

use super::{compose_id, docker, read, report};

/// Prepares Compose's Postgres for a replica of `image`, with its `bootstrap` and `migrate`, and
/// returns the network it is on and the replica's settings.
pub(super) fn prepare(root: &Path, image: &str) -> Result<(String, Vec<String>), String> {
    let network = docker(
        root,
        &[
            "inspect",
            "--format",
            "{{range $name, $_ := .NetworkSettings.Networks}}{{$name}}{{end}}",
            &compose_id(root, "postgres"),
        ],
    )
    .map_err(|error| format!("Postgres isn't running; run `just services`: {error}"))?;
    let settings = settings(root)?;
    for task in ["bootstrap", "migrate"] {
        let mut args = vec!["run", "--rm", "--network", &network];
        for setting in &settings {
            // Compose's Postgres holds the development database and the tests', which share the
            // roles, and in production `bootstrap` refuses a server that holds another database
            // of the roles (PLATFORM-003), so it runs as development's.
            let setting = match (task, setting.as_str()) {
                ("bootstrap", "ENVIRONMENT=production") => "ENVIRONMENT=development",
                _ => setting,
            };
            args.extend(["--env", setting]);
        }
        args.extend([image, task]);
        docker(root, &args).map_err(|error| format!("{task} failed: {error}"))?;
    }
    Ok((network, settings))
}

/// Waits until Docker's health check finds the container `id` healthy, following its events, as
/// the end-to-end tests do.
pub(super) fn healthy(root: &Path, id: &str) -> Result<(), String> {
    let created = docker(root, &["inspect", "--format", "{{.Created}}", id])?;
    let mut events = Command::new("docker")
        .args([
            "events",
            "--since",
            &created,
            "--filter",
            &format!("container={id}"),
            "--filter",
            "event=health_status",
            "--filter",
            "event=die",
            "--format",
            "{{.Action}}",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't follow the replica's events: {error}"))?;
    let stdout = events.stdout.take().ok_or("no events")?;
    let verdict = BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
        .find(|status| status.contains("healthy") || status.trim() == "die");
    // The events end once the verdict is read.
    let stopped = events.kill().and_then(|()| events.wait());
    if let Err(error) = stopped {
        report(&format!("couldn't stop following the events: {error}"));
    }
    match verdict.as_deref().map(str::trim) {
        Some("health_status: healthy") => Ok(()),
        other => Err(format!("the replica didn't become healthy: {other:?}")),
    }
}

/// The label of each container that the load test starts, as of each that the end-to-end tests
/// start in the background (crates/app/tests/e2e/machine.rs), by which a later run of either finds
/// what an earlier one left.
pub(super) const LABEL: &str = "image-test-container";

/// Holds the machine for the load test, through the lock that a run of the end-to-end tests holds
/// it by (crates/app/tests/e2e/machine.rs), then removes the containers that an earlier run of
/// either left, with their volumes; or says why it can't: a run of either counts the machine as
/// its own (OPS-21). The operating system lets the machine go when the process ends.
pub(super) fn hold_the_machine(root: &Path) -> Result<File, String> {
    let machine = hold(&std::env::temp_dir().join("image-tests.lock"))?;
    let left = docker(
        root,
        &[
            "ps",
            "--all",
            "--quiet",
            "--filter",
            &format!("label={LABEL}"),
        ],
    )?;
    let left: Vec<&str> = left.lines().collect();
    if !left.is_empty() {
        docker(root, &[&["rm", "--force", "--volumes"][..], &left].concat())?;
    }
    Ok(machine)
}

/// Starts the observability stack that the replica exports its telemetry to, once the load test
/// holds the machine, whose memory the stack takes.
pub(super) fn start_the_stack(root: &Path) -> Result<(), String> {
    docker(
        root,
        &[
            "compose",
            "--profile",
            "observability",
            "up",
            "--detach",
            "--wait",
            "collector",
            "prometheus",
            "loki",
            "tempo",
        ],
    )
    .map(|_| ())
    .map_err(|error| format!("the observability stack didn't start: {error}"))
}

/// The lock on the file at `path`, or why there is none: another process holds it, or the file
/// can't be opened.
#[expect(
    clippy::disallowed_methods,
    reason = "the load test runs as a synchronous program, with no async runtime to stall"
)]
fn hold(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|error| format!("can't open {}: {error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(format!(
            "a run of the end-to-end tests or of the load test holds this machine, as its lock on \
             {} says: wait for it to end, or end it",
            path.display()
        )),
        Err(TryLockError::Error(error)) => Err(format!("can't lock {}: {error}", path.display())),
    }
}

/// The image for this machine's platform, of the index in target/image, which it pushes to the
/// registry that Compose runs, emptied first, as this machine reaches it there.
pub(super) fn image(root: &Path) -> Result<String, String> {
    docker(
        root,
        &[
            "compose",
            "rm",
            "--force",
            "--stop",
            "--volumes",
            "registry",
        ],
    )?;
    docker(root, &["compose", "up", "--detach", "--wait", "registry"])?;
    docker(
        root,
        &[
            "compose",
            "run",
            "--rm",
            "--no-TTY",
            "crane",
            "push",
            "--insecure",
            "/image",
            "registry:5000/image:e2e",
        ],
    )?;
    let index: Value = serde_json::from_str(
        &read(&root.join("target/image/index.json"))
            .map_err(|_| "target/image/index.json is missing; run `just image`".to_owned())?,
    )
    .map_err(|error| format!("target/image/index.json isn't an index: {error}"))?;
    let index = index["manifests"][0]["digest"]
        .as_str()
        .ok_or("target/image/index.json names no image")?;
    let platform = if std::env::consts::ARCH == "aarch64" {
        "linux/arm64"
    } else {
        "linux/amd64"
    };
    let digest = docker(
        root,
        &[
            "compose",
            "run",
            "--rm",
            "--no-TTY",
            "crane",
            "digest",
            "--insecure",
            "--platform",
            platform,
            &format!("registry:5000/image@{index}"),
        ],
    )?;
    let registry = docker(root, &["compose", "port", "registry", "5000"])?;
    Ok(format!("{registry}/image@{digest}"))
}

/// The CPUs and the memory of one replica, as deploy/settings.env.example sets them by default.
pub(super) fn limits(root: &Path) -> Result<(String, String), String> {
    let example = read(&root.join("deploy/settings.env.example"))
        .map_err(|error| format!("can't read deploy/settings.env.example: {error}"))?;
    let value = |name: &str| {
        example
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .map(str::to_owned)
            .ok_or(format!("deploy/settings.env.example sets no {name}"))
    };
    Ok((value("API_CPUS")?, value("API_MEMORY")?))
}

/// The settings of the image run alone, which the end-to-end tests of the image share, so a
/// setting that `serve` comes to require fails those tests, which CI runs, until it is there.
const IMAGE_SETTINGS: &str = "crates/app/tests/e2e/image.env";

/// The replica's settings: those of `IMAGE_SETTINGS`, the database's, and the HMAC key and the encryption keys of .env,
/// with Compose's Postgres as `postgres`, and IP, user and sign-in limits that the load, from
/// one address and a few users, never reaches, so the test measures the replica rather than the
/// limits, while every request still counts in them.
pub(super) fn settings(root: &Path) -> Result<Vec<String>, String> {
    let key = std::env::var("HMAC_KEY").map_err(|_| "HMAC_KEY is unset; run `just setup`")?;
    let keys = std::env::var("ENCRYPTION_KEYS")
        .map_err(|_| "ENCRYPTION_KEYS is unset; run `just setup`")?;
    let mut settings = image_settings(root)?;
    settings.extend([
        format!("HMAC_KEY={key}"),
        format!("ENCRYPTION_KEYS={keys}"),
        "RATE_LIMIT_IP_REQUESTS=1000000".to_owned(),
        "RATE_LIMIT_USER_REQUESTS=1000000".to_owned(),
        // The sign-in limits, of an email from one address and from every address: the most
        // that each takes, in a window of a second (AUTH-006).
        "SIGN_IN_ATTEMPTS=1000".to_owned(),
        "SIGN_IN_EMAIL_ATTEMPTS=100000".to_owned(),
        "SIGN_IN_WINDOW_SECONDS=1".to_owned(),
    ]);
    for name in ["DATABASE_ADMIN_URL", "DATABASE_OWNER_URL", "DATABASE_URL"] {
        let value =
            std::env::var(name).map_err(|_| format!("{name} is unset; run `just setup`"))?;
        let at = value.rfind('@').ok_or(format!("{name} has no host"))?;
        let rest = &value[at + 1..];
        let path = rest.find('/').map_or("", |slash| &rest[slash..]);
        settings.push(format!("{name}={}@postgres:5432{path}", &value[..at]));
    }
    Ok(settings)
}

/// The settings of `IMAGE_SETTINGS`: each line that is neither empty nor a comment.
fn image_settings(root: &Path) -> Result<Vec<String>, String> {
    let settings = read(&root.join(IMAGE_SETTINGS))
        .map_err(|error| format!("can't read {IMAGE_SETTINGS}: {error}"))?;
    Ok(settings
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_16_the_replica_takes_the_settings_of_the_image_run_alone() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let settings = image_settings(&root).unwrap();
        assert!(
            settings
                .iter()
                .any(|setting| setting.starts_with("ENVIRONMENT=")),
            "{settings:?}"
        );
        assert!(
            settings.iter().all(|setting| setting
                .split_once('=')
                .is_some_and(|(name, _)| !name.is_empty())),
            "{settings:?}"
        );
    }

    #[test]
    fn ops_21_a_load_test_holds_the_machine_so_that_another_fails_at_once() {
        // A file of this test's own, so a run of the end-to-end tests that holds the machine
        // meanwhile changes nothing.
        let path = std::env::temp_dir().join(format!("image-tests-{}.lock", std::process::id()));

        let first = hold(&path);
        let second = hold(&path);

        assert!(first.is_ok(), "{first:?}");
        assert!(
            second
                .as_ref()
                .is_err_and(|refused| refused.contains("holds this machine")),
            "{second:?}"
        );
    }
}
