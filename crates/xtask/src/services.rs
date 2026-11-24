//! `stale-services`, which `just services` runs before it starts the development services: it
//! restarts each running service whose configuration changed since it started, so that no
//! developer restarts one by hand after a change (OPS-20). Compose's `up` leaves a running
//! container as it is when only a file that it mounts changed, and a container shows a file
//! mounted by itself as it was when the container started, even once the file is replaced.
//!
//! A service's configuration is what Compose's resolved file gives it as a bind mount whose
//! source is under the repository's `deploy/`: a directory, with every file and directory below
//! it, or a file. It changed when one of those files or directories was modified after the moment
//! at which the service's container last started, as Docker names it, each as precisely as Docker
//! and the filesystem give it. A directory is modified when an entry is added to it, removed from
//! it or renamed in it, so a file deleted from a mounted directory is a change, and so is one
//! moved into it with a time before the start. A file mounted by itself that is replaced by
//! another whose time is before the start is no change. A service without a container, or whose
//! container doesn't run, reads its configuration when a task starts it.

use std::{
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{Duration, UNIX_EPOCH},
};

use serde_json::Value;

/// Every profile of the Compose file, so that the services of each are read.
const PROFILES: [&str; 2] = ["--profile", "*"];

/// Restarts the running services whose configuration changed since they started.
pub fn run() -> ExitCode {
    // The repository's root, two directories above this crate's, written without `..`, so that
    // a path that Compose resolved below it starts with it.
    let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2) else {
        return ExitCode::FAILURE;
    };
    match restart_stale(root) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            #[expect(clippy::print_stderr, reason = "the error is the tool's output")]
            {
                eprintln!("can't tell which services read a changed configuration: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn restart_stale(root: &Path) -> Result<(), String> {
    let config = compose(root, &["config", "--format", "json"])?;
    let config: Value = serde_json::from_str(&config)
        .map_err(|error| format!("Compose's configuration isn't JSON: {error}"))?;
    for (service, sources) in mounted(&config, &root.join("deploy")) {
        let id = compose(root, &["ps", "--quiet", &service])?;
        let Some(id) = id.lines().next().filter(|id| !id.is_empty()) else {
            continue;
        };
        let started = docker(&[
            "inspect",
            "--format",
            "{{.State.Running}} {{.State.StartedAt}}",
            id,
        ])?;
        let Some(started) = started
            .strip_prefix("true ")
            .and_then(|started| moment(started.trim()))
        else {
            continue;
        };
        if changed(&sources, started) {
            #[expect(clippy::print_stdout, reason = "the line is the tool's output")]
            {
                println!("{service}'s configuration changed since it started; restarting it");
            }
            compose(root, &["restart", &service])?;
        }
    }
    Ok(())
}

/// Each service of `config`, Compose's resolved file, with the sources of its bind mounts that
/// are `deploy` or below it.
fn mounted(config: &Value, deploy: &Path) -> Vec<(String, Vec<PathBuf>)> {
    let services = config["services"].as_object().into_iter().flatten();
    services
        .filter_map(|(name, service)| {
            let sources: Vec<PathBuf> = service["volumes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|volume| volume["type"] == "bind")
                .filter_map(|volume| volume["source"].as_str())
                .map(PathBuf::from)
                .filter(|source| source.starts_with(deploy))
                .collect();
            (!sources.is_empty()).then(|| (name.clone(), sources))
        })
        .collect()
}

/// Whether a file or a directory at one of `sources`, or below it, was modified after `started`,
/// a moment since 1970.
fn changed(sources: &[PathBuf], started: Duration) -> bool {
    let modified = sources.iter().filter_map(|source| newest(source)).max();
    modified.is_some_and(|modified| modified > started)
}

/// The moment, since 1970, at which the newest file or directory at `path`, or below it, was
/// modified, or `None` where nothing is there.
#[expect(
    clippy::disallowed_methods,
    reason = "the task runs as a synchronous program, with no async runtime to stall"
)]
fn newest(path: &Path) -> Option<Duration> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok());
    if !metadata.is_dir() {
        return modified;
    }
    let below = std::fs::read_dir(path)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| newest(&entry.path()))
        .max();
    modified.max(below)
}

/// The moment, since 1970, of `time`, a moment in UTC as Docker writes one, such as
/// `2026-11-22T00:51:44.132126703Z`, with a fraction of 1 to 9 digits or none, or `None` for
/// another form.
fn moment(time: &str) -> Option<Duration> {
    let (date, clock) = time.strip_suffix('Z')?.split_once('T')?;
    let numbers = |text: &str, separator: char| -> Option<Vec<u64>> {
        text.split(separator)
            .map(|part| {
                (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| part.parse().ok())
                    .flatten()
            })
            .collect()
    };
    // The fraction of the second, from tenths to nanoseconds.
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, "0"));
    let digits = u32::try_from(fraction.len())
        .ok()
        .filter(|digits| (1..=9).contains(digits))?;
    let fraction: u32 = fraction
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| fraction.parse().ok())
        .flatten()?;
    let (date, clock) = (numbers(date, '-')?, numbers(clock, ':')?);
    let ([year, month, day], [hour, minute, second]) = (date.as_slice(), clock.as_slice()) else {
        return None;
    };
    if !(1..=12).contains(month) || !(1..=31).contains(day) || *year < 1970 {
        return None;
    }
    // Howard Hinnant's algorithm from the civil calendar to days, whose years start in March.
    let year = if *month <= 2 { year - 1 } else { *year };
    let era = year / 400;
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = (era * 146_097 + day_of_era).checked_sub(719_468)?;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(Duration::new(seconds, fraction * 10u32.pow(9 - digits)))
}

/// What `docker compose`, with every profile, wrote for `args`, run at `root`.
fn compose(root: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("docker");
    command.arg("compose").args(PROFILES).args(args);
    command.current_dir(root);
    written(command, &format!("docker compose {}", args.join(" ")))
}

/// What `docker` wrote for `args`.
fn docker(args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("docker");
    command.args(args);
    written(command, &format!("docker {}", args.join(" ")))
}

/// What `command`, named `name` in a failure, wrote to its standard output.
fn written(mut command: Command, name: &str) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|error| format!("can't run `{name}`: {error}"))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(format!("`{name}` failed: {}", error.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::calendar::civil;

    #[test]
    fn ops_20_a_service_s_configuration_is_what_it_mounts_from_deploy() {
        let config = json!({ "services": {
            "collector": { "volumes": [
                { "type": "bind", "source": "/repo/deploy/observability/collector" },
                { "type": "bind", "source": "/" },
                { "type": "volume", "source": "data" },
            ] },
            "gadget": { "volumes": [
                { "type": "bind", "source": "/repo/deploy/gadget/gadget.conf" },
                { "type": "bind", "source": "/repo/deploy/gadget/names.txt" },
            ] },
            "crane": { "volumes": [{ "type": "bind", "source": "/repo/target/image" }] },
            "beside": { "volumes": [{ "type": "bind", "source": "/repo/deployed/x" }] },
            "postgres": {},
        } });

        let mounted = mounted(&config, Path::new("/repo/deploy"));

        assert_eq!(
            mounted,
            [
                (
                    "collector".to_owned(),
                    vec![PathBuf::from("/repo/deploy/observability/collector")]
                ),
                (
                    "gadget".to_owned(),
                    vec![
                        PathBuf::from("/repo/deploy/gadget/gadget.conf"),
                        PathBuf::from("/repo/deploy/gadget/names.txt")
                    ]
                ),
            ]
        );
    }

    #[test]
    fn ops_20_the_moment_a_container_started_is_read_from_docker_s_time() {
        for (time, written, nanos) in [
            (
                "2026-11-22T00:51:44.132126703Z",
                "20261122005144",
                132_126_703,
            ),
            ("2024-02-29T23:59:59Z", "20240229235959", 0),
            ("1970-01-01T00:00:00.5Z", "19700101000000", 500_000_000),
            ("2000-03-01T12:00:00.000000001Z", "20000301120000", 1),
        ] {
            let read = moment(time);

            assert_eq!(
                read.map(|read| (civil(read.as_secs()), read.subsec_nanos())),
                Some((written.to_owned(), nanos)),
                "{time}"
            );
        }
        for unread in [
            "0001-01-01T00:00:00Z",
            "2026-11-22T00:51:44+01:00",
            "2026-11-22 00:51:44Z",
            "2026-13-22T00:51:44Z",
            "2026-11-22T00:51Z",
            "2026-11-+2T00:51:44Z",
            "2026-11-22T00:51:44.Z",
            "2026-11-22T00:51:44.+5Z",
            "2026-11-22T00:51:44.1321267031Z",
            "",
        ] {
            assert_eq!(moment(unread), None, "{unread}");
        }
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test writes a file of its own, outside any async runtime"
    )]
    fn ops_20_a_file_modified_within_the_second_its_service_started_but_after_it_is_a_change() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/stale-services")
            .join(std::process::id().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("collector.yaml");
        let written = std::fs::File::create(&file).unwrap();
        let second = moment("2026-11-22T00:51:44Z").unwrap();
        written
            .set_modified(UNIX_EPOCH + second + Duration::from_millis(700))
            .unwrap();
        drop(written);
        // The directory, whose entry the file's creation modified, was last modified before both
        // starts.
        std::fs::File::open(&root)
            .unwrap()
            .set_modified(UNIX_EPOCH + second)
            .unwrap();
        let sources = [root.clone()];

        let after = changed(&sources, moment("2026-11-22T00:51:44.3Z").unwrap());
        let before = changed(&sources, moment("2026-11-22T00:51:44.9Z").unwrap());
        std::fs::remove_dir_all(&root).unwrap();

        assert!(
            after,
            "a file modified at .7 after a start at .3 is a change"
        );
        assert!(
            !before,
            "a file modified at .7 before a start at .9 is none"
        );
    }

    /// A directory of the test's own, named `name`, in `target/`, which holds the files `files`,
    /// each, with the directory itself, last modified at `2026-11-22T00:51:44Z`.
    #[expect(
        clippy::disallowed_methods,
        reason = "the test writes files of its own, outside any async runtime"
    )]
    fn mounted_directory(name: &str, files: &[&str]) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/stale-services")
            .join(format!("{}-{name}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let at = UNIX_EPOCH + moment("2026-11-22T00:51:44Z").unwrap();
        for file in files {
            std::fs::File::create(root.join(file))
                .unwrap()
                .set_modified(at)
                .unwrap();
        }
        std::fs::File::open(&root)
            .unwrap()
            .set_modified(at)
            .unwrap();
        root
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test writes files of its own, outside any async runtime"
    )]
    fn ops_20_a_file_deleted_from_a_mounted_directory_after_its_service_started_is_a_change() {
        let root = mounted_directory("deleted", &["kept.yaml", "deleted.yaml"]);
        let (sources, started) = ([root.clone()], moment("2026-11-22T00:51:44.3Z").unwrap());
        let unchanged = changed(&sources, started);

        std::fs::remove_file(root.join("deleted.yaml")).unwrap();
        let deleted = changed(&sources, started);
        std::fs::remove_dir_all(&root).unwrap();

        assert!(
            !unchanged,
            "nothing below the directory changed after the start"
        );
        assert!(deleted, "a file deleted after the start is a change");
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test writes files of its own, outside any async runtime"
    )]
    fn ops_20_a_file_moved_into_a_mounted_directory_with_its_earlier_time_is_a_change() {
        let root = mounted_directory("moved", &["kept.yaml"]);
        let outside = root.with_extension("outside");
        std::fs::File::create(&outside)
            .unwrap()
            .set_modified(UNIX_EPOCH + moment("2026-11-22T00:51:44Z").unwrap())
            .unwrap();
        let (sources, started) = ([root.clone()], moment("2026-11-22T00:51:44.3Z").unwrap());
        let unchanged = changed(&sources, started);

        std::fs::rename(&outside, root.join("moved.yaml")).unwrap();
        let moved = changed(&sources, started);
        std::fs::remove_dir_all(&root).unwrap();

        assert!(
            !unchanged,
            "nothing below the directory changed after the start"
        );
        assert!(
            moved,
            "a file moved in after the start, with a time before it, is a change"
        );
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test writes files of its own, outside any async runtime"
    )]
    fn ops_20_a_file_mounted_by_itself_that_one_with_a_time_before_the_start_replaces_is_none() {
        let root = mounted_directory("replaced", &["mounted.yaml", "other.yaml"]);
        let started = moment("2026-11-22T00:51:44.3Z").unwrap();

        std::fs::rename(root.join("other.yaml"), root.join("mounted.yaml")).unwrap();
        let replaced = changed(&[root.join("mounted.yaml")], started);
        let directory = changed(std::slice::from_ref(&root), started);
        std::fs::remove_dir_all(&root).unwrap();

        assert!(
            !replaced,
            "the file mounted by itself keeps a time before the start"
        );
        assert!(directory, "the directory that holds it is modified");
    }
}
