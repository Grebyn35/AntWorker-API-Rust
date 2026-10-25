//! `check-pins`: everything a build or a check uses is pinned (OPS-13), and the toolchain and the
//! edition are the ones `rust-toolchain.toml` and the workspace name (CODE-01).

use crate::{Finding, Outcome, repo::Repo};

const WORKFLOWS: &str = ".github/workflows/";
const COMPOSE: &str = "compose.yaml";
const DOCKERFILE: &str = "Dockerfile";
const JUSTFILE: &str = "justfile";
const TOOLCHAIN: &str = "rust-toolchain.toml";
const WORKSPACE: &str = "Cargo.toml";

/// Runs the check.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let just_version = justfile(repo, &mut findings);
    for file in repo.files() {
        let workflow = file.path.starts_with(WORKFLOWS)
            && (file.path.ends_with(".yml") || file.path.ends_with(".yaml"));
        if workflow {
            workflow_pins(
                &file.path,
                &file.text,
                just_version.as_deref(),
                &mut findings,
            );
        }
        let member = file.path.starts_with("crates/") && file.path.ends_with("/Cargo.toml");
        if member && !has_line(&file.text, "edition.workspace = true") {
            findings.push(Finding::new(
                &file.path,
                1,
                "a crate takes its edition from the workspace (CODE-01)",
            ));
        }
    }
    for file in repo.files() {
        let name = file.path.rsplit('/').next().unwrap_or_default();
        if name == COMPOSE {
            compose_pins(&file.path, &file.text, &mut findings);
        }
        if name == DOCKERFILE {
            dockerfile_pins(&file.path, &file.text, &mut findings);
        }
    }
    toolchain(repo, &mut findings);
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// Checks the tool versions the justfile pins, and returns the version of `just` itself.
fn justfile(repo: &Repo, findings: &mut Vec<Finding>) -> Option<String> {
    let Some(file) = repo.file(JUSTFILE) else {
        findings.push(Finding::new(JUSTFILE, 1, "the justfile is missing"));
        return None;
    };
    let mut just = None;
    for (number, line) in numbered(&file.text) {
        let Some((name, value)) = line.split_once(":=") else {
            continue;
        };
        let name = name.trim();
        if !name.ends_with("_version") {
            continue;
        }
        let version = value.trim().trim_matches('"');
        if !is_exact_version(version) {
            findings.push(Finding::new(
                JUSTFILE,
                number,
                format!("{name} is an exact version, such as 1.2.3 (OPS-13)"),
            ));
        }
        if name == "just_version" {
            just = Some(version.to_owned());
        }
    }
    just
}

fn workflow_pins(path: &str, text: &str, just_version: Option<&str>, findings: &mut Vec<Finding>) {
    for (number, line) in numbered(text) {
        let line = line.trim_start().trim_start_matches("- ");
        if let Some(action) = value_of(line, "uses:") {
            if let Some(problem) = action_problem(action) {
                findings.push(Finding::new(path, number, problem));
            }
        } else if let Some(runner) = value_of(line, "runs-on:") {
            if !is_versioned_runner(runner) {
                findings.push(Finding::new(
                    path,
                    number,
                    format!(
                        "the runner {runner} is a versioned label, such as ubuntu-24.04 (OPS-13)"
                    ),
                ));
            }
        } else if let Some(image) = value_of(line, "image:") {
            // A key with no value, such as a job named `image`, names no image.
            if !image.is_empty() && !has_digest(image) {
                findings.push(Finding::new(
                    path,
                    number,
                    format!("the image {image} is pinned by digest (OPS-13)"),
                ));
            }
        } else if let Some(version) =
            value_of(line, "tool:").and_then(|tool| tool.strip_prefix("just@"))
            && just_version.is_some_and(|pinned| pinned != version)
        {
            findings.push(Finding::new(
                path,
                number,
                format!("just@{version} differs from the justfile's just_version (OPS-13)"),
            ));
        }
        if line.contains("rustup") || line.contains("cargo +") {
            findings.push(Finding::new(
                path,
                number,
                "a workflow installs or picks no toolchain: `just setup` installs the one \
                 rust-toolchain.toml pins (CODE-01)",
            ));
        }
    }
}

/// What is wrong with the action a step uses, if anything.
fn action_problem(action: &str) -> Option<String> {
    if action.starts_with("./") {
        return None;
    }
    if let Some(image) = action.strip_prefix("docker://") {
        return (!has_digest(image))
            .then(|| format!("the image {image} is pinned by digest (OPS-13)"));
    }
    let pinned = action
        .rsplit_once('@')
        .is_some_and(|(_, sha)| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()));
    if !pinned {
        return Some(format!(
            "the action {action} is pinned by the SHA of a commit (OPS-13)"
        ));
    }
    let name = action.split('@').next().unwrap_or_default();
    name.contains("toolchain").then(|| {
        format!(
            "the action {name} installs a toolchain: `just setup` installs the one \
             rust-toolchain.toml pins (CODE-01)"
        )
    })
}

/// Checks the images that the compose file at `path` names.
fn compose_pins(path: &str, text: &str, findings: &mut Vec<Finding>) {
    for (number, line) in numbered(text) {
        if let Some(image) = value_of(line.trim_start(), "image:")
            && !has_digest(image)
        {
            findings.push(Finding::new(
                path,
                number,
                format!("the image {image} is pinned by digest (OPS-13)"),
            ));
        }
    }
}

/// Checks the frontend, the images, the downloads, the packages, the crates and the toolchain of
/// the Dockerfile at `path`.
fn dockerfile_pins(path: &str, text: &str, findings: &mut Vec<Finding>) {
    let mut stages: Vec<String> = Vec::new();
    let mut syntax = false;
    for (number, line) in numbered(text) {
        let line = line.trim();
        if let Some(frontend) = line.strip_prefix("# syntax=") {
            syntax = true;
            if !has_digest(frontend) {
                findings.push(Finding::new(
                    path,
                    number,
                    format!("the frontend {frontend} is pinned by digest (OPS-13)"),
                ));
            }
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        match words
            .first()
            .map(|word| word.to_ascii_uppercase())
            .as_deref()
        {
            Some("FROM") => {
                let mut rest = words[1..].iter().skip_while(|word| word.starts_with("--"));
                let image = rest.next().copied().unwrap_or_default();
                if !has_digest(image) && !names_stage(image, &stages) {
                    findings.push(Finding::new(
                        path,
                        number,
                        format!("the image {image} is pinned by digest (OPS-13)"),
                    ));
                }
                if let (Some(as_), Some(stage)) = (rest.next(), rest.next())
                    && as_.eq_ignore_ascii_case("as")
                {
                    stages.push((*stage).to_owned());
                }
            }
            Some("ADD") => {
                let remote = words
                    .iter()
                    .any(|word| word.starts_with("https://") || word.starts_with("http://"));
                if remote
                    && !words
                        .iter()
                        .any(|word| word.starts_with("--checksum=sha256:"))
                {
                    findings.push(Finding::new(
                        path,
                        number,
                        "a download is pinned by its checksum, with --checksum=sha256: (OPS-13)",
                    ));
                }
            }
            _ => {}
        }
        for problem in command_problems(line, &words) {
            findings.push(Finding::new(path, number, problem));
        }
    }
    if !syntax {
        findings.push(Finding::new(
            path,
            1,
            "the first line pins the Dockerfile frontend, with `# syntax=` and a digest (OPS-13)",
        ));
    }
}

/// What is wrong with the commands of the Dockerfile line `line`, split into `words`: a crate
/// or a package installed without its version, or a toolchain other than the pinned one.
fn command_problems(line: &str, words: &[&str]) -> Vec<String> {
    let mut problems: Vec<String> = cargo_install_problem(words).into_iter().collect();
    for package in unpinned_packages(words) {
        problems.push(format!(
            "the package {package} is pinned by version, as {package}=<version> (OPS-13)"
        ));
    }
    if line.contains("cargo +") || words.windows(3).any(toolchain_named) {
        problems.push(
            "the image builds with the toolchain rust-toolchain.toml pins (CODE-01)".to_owned(),
        );
    }
    problems
}

/// Whether `image` names an earlier stage, such as `tools`, or one per platform, such as
/// `zig-$BUILDARCH` for the stages `zig-amd64` and `zig-arm64`.
fn names_stage(image: &str, stages: &[String]) -> bool {
    match image.split_once('$') {
        None => stages.iter().any(|stage| stage == image),
        Some((prefix, _)) => ["amd64", "arm64"].iter().all(|arch| {
            stages
                .iter()
                .any(|stage| *stage == format!("{prefix}{arch}"))
        }),
    }
}

/// The packages that an `apt-get install` among `words` names without a version.
fn unpinned_packages<'a>(words: &[&'a str]) -> Vec<&'a str> {
    let Some(at) = words
        .windows(2)
        .position(|pair| pair == ["apt-get", "install"])
    else {
        return Vec::new();
    };
    words[at + 2..]
        .iter()
        .take_while(|word| !["&&", ";", "\\", "||"].contains(word))
        .filter(|word| !word.starts_with('-') && !word.contains('='))
        .copied()
        .collect()
}

/// What is wrong with a `cargo install` among `words`, if anything.
fn cargo_install_problem(words: &[&str]) -> Option<String> {
    let at = words
        .windows(2)
        .position(|pair| pair == ["cargo", "install"])?;
    let args = words[at + 2..]
        .iter()
        .take_while(|word| !matches!(**word, "&&" | ";" | "\\" | "|"));
    let mut locked = false;
    let mut problems = Vec::new();
    for arg in args {
        if *arg == "--locked" {
            locked = true;
        } else if !arg.starts_with('-') {
            let exact = arg
                .split_once('@')
                .is_some_and(|(_, version)| is_exact_version(version));
            if !exact {
                problems.push(format!("{arg} names an exact version, as name@1.2.3"));
            }
        }
    }
    if !locked {
        problems.push("cargo install takes --locked".to_owned());
    }
    (!problems.is_empty()).then(|| format!("{} (OPS-13)", problems.join("; ")))
}

/// Whether three words are `rustup toolchain install <toolchain>` or `rustup default
/// <toolchain>`, which would pick a toolchain other than rust-toolchain.toml's.
fn toolchain_named(words: &[&str]) -> bool {
    match words {
        ["rustup", "default", _] => true,
        ["toolchain", "install", toolchain] => !matches!(*toolchain, "&&" | ";" | "\\"),
        _ => false,
    }
}

fn toolchain(repo: &Repo, findings: &mut Vec<Finding>) {
    match repo.file(TOOLCHAIN) {
        Some(file) => {
            let channel = file
                .text
                .lines()
                .find_map(|line| value_of(line.trim(), "channel ="))
                .map(|channel| channel.trim_matches('"'));
            if !channel.is_some_and(is_exact_version) {
                findings.push(Finding::new(
                    TOOLCHAIN,
                    1,
                    "the channel is an exact version, such as 1.99.1 (CODE-01)",
                ));
            }
        }
        None => findings.push(Finding::new(TOOLCHAIN, 1, "the toolchain file is missing")),
    }
    if !repo
        .file(WORKSPACE)
        .is_some_and(|file| has_line(&file.text, "edition = \"2024\""))
    {
        findings.push(Finding::new(
            WORKSPACE,
            1,
            "the workspace's edition is 2024 (CODE-01)",
        ));
    }
}

/// Each line of `text` with its number, counted from 1.
fn numbered(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
}

/// The value after `key` at the start of `line`, without quotes or a trailing comment.
fn value_of<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let value = line.strip_prefix(key)?;
    let value = value.split(" #").next().unwrap_or_default().trim();
    Some(value.trim_matches(|c| c == '"' || c == '\''))
}

fn has_line(text: &str, wanted: &str) -> bool {
    text.lines().any(|line| line.trim() == wanted)
}

fn has_digest(image: &str) -> bool {
    image.rsplit_once("@sha256:").is_some_and(|(_, digest)| {
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// Whether `version` is three whole numbers, such as `1.99.1`.
fn is_exact_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `runner` is a hosted runner's versioned label, such as `ubuntu-24.04` or
/// `ubuntu-24.04-arm`.
fn is_versioned_runner(runner: &str) -> bool {
    let runner = runner.strip_suffix("-arm").unwrap_or(runner);
    ["ubuntu-", "windows-", "macos-"].iter().any(|os| {
        runner.strip_prefix(os).is_some_and(|version| {
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn pinned() -> Vec<(String, String)> {
        vec![
            (
                JUSTFILE.to_owned(),
                "just_version := \"1.59.0\"\ncargo_deny_version := \"0.20.2\"\n".to_owned(),
            ),
            (
                TOOLCHAIN.to_owned(),
                "[toolchain]\nchannel = \"1.99.1\"\n".to_owned(),
            ),
            (
                WORKSPACE.to_owned(),
                "[workspace.package]\nedition = \"2024\"\n".to_owned(),
            ),
            (
                "crates/app/Cargo.toml".to_owned(),
                "[package]\nedition.workspace = true\n".to_owned(),
            ),
            (
                ".github/workflows/ci.yml".to_owned(),
                format!(
                    "jobs:\n  checks:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@{SHA} # v7.0.1\n      - uses: taiki-e/install-action@{SHA}\n        with:\n          tool: just@1.59.0\n      - run: just ci\n  arm:\n    runs-on: ubuntu-24.04-arm\n"
                ),
            ),
            (
                COMPOSE.to_owned(),
                format!("services:\n  postgres:\n    image: postgres:18.6@sha256:{DIGEST}\n"),
            ),
            (
                DOCKERFILE.to_owned(),
                format!(
                    "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nFROM --platform=$BUILDPLATFORM alpine:3.24.3@sha256:{DIGEST} AS zig-amd64\nADD --checksum=sha256:{DIGEST} https://example.org/zig.tar.xz /zig.tar.xz\nFROM alpine:3.24.3@sha256:{DIGEST} AS zig-arm64\nFROM zig-$BUILDARCH AS zig\nFROM rust:1.99.1@sha256:{DIGEST} AS tools\nRUN cargo install --locked cargo-chef@0.1.78 cargo-zigbuild@0.24.0\nRUN rustup toolchain install && rustup target add x86_64-unknown-linux-musl\nFROM tools AS build\n"
                ),
            ),
        ]
    }

    fn findings_with(path: &str, text: &str) -> Vec<String> {
        let mut files = pinned();
        files.retain(|(existing, _)| existing != path);
        files.push((path.to_owned(), text.to_owned()));
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        check(&Repo::of(&files))
            .findings
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn ops_13_every_dockerfile_and_compose_file_is_checked() {
        let dockerfile = findings_with(
            "deploy/postgres/Dockerfile",
            "# syntax=docker/dockerfile:1\nFROM postgres:18.6\n",
        );
        let compose = findings_with(
            "deploy/compose.yaml",
            "services:\n  caddy:\n    image: caddy:2.11.7\n",
        );

        assert_eq!(
            dockerfile,
            [
                "deploy/postgres/Dockerfile:1: the frontend docker/dockerfile:1 is pinned by digest (OPS-13)",
                "deploy/postgres/Dockerfile:2: the image postgres:18.6 is pinned by digest (OPS-13)",
            ]
        );
        assert_eq!(
            compose,
            ["deploy/compose.yaml:3: the image caddy:2.11.7 is pinned by digest (OPS-13)"]
        );
    }

    #[test]
    fn ops_13_a_package_the_image_installs_is_pinned_by_version() {
        let findings = findings_with(
            "deploy/postgres/Dockerfile",
            &format!(
                "# syntax=docker/dockerfile:1@sha256:{DIGEST}\nFROM postgres:18.6@sha256:{DIGEST}\nRUN apt-get update && apt-get install --yes pgbackrest=2.59.3-1 curl && rm -rf /var/lib/apt/lists/*\n"
            ),
        );

        assert_eq!(
            findings,
            [
                "deploy/postgres/Dockerfile:3: the package curl is pinned by version, as curl=<version> (OPS-13)"
            ]
        );
    }

    #[test]
    fn ops_13_a_pinned_repository_passes() {
        let files = pinned();
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();

        assert_eq!(check(&Repo::of(&files)).findings, []);
    }

    #[test]
    fn ops_13_an_action_is_pinned_by_a_commit_s_sha() {
        let findings = findings_with(
            ".github/workflows/ci.yml",
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v7\n      - uses: ./local\n",
        );

        assert_eq!(
            findings,
            [
                ".github/workflows/ci.yml:5: the action actions/checkout@v7 is pinned by the SHA of a commit (OPS-13)"
            ]
        );
    }

    #[test]
    fn ops_13_an_image_a_workflow_names_is_pinned_by_digest() {
        let findings = findings_with(
            ".github/workflows/ci.yml",
            "jobs:
  image:
    runs-on: ubuntu-24.04
    services:
      db:
        image: postgres:18
",
        );

        assert_eq!(
            findings,
            [".github/workflows/ci.yml:6: the image postgres:18 is pinned by digest (OPS-13)"]
        );
    }

    #[test]
    fn ops_13_a_runner_is_a_versioned_label() {
        for runner in ["ubuntu-latest", "${{ matrix.os }}", "self-hosted"] {
            let findings = findings_with(
                ".github/workflows/ci.yml",
                &format!("jobs:\n  a:\n    runs-on: {runner}\n"),
            );

            assert_eq!(findings.len(), 1, "{runner}: {findings:?}");
            assert!(findings[0].contains("versioned label"), "{findings:?}");
        }
    }

    #[test]
    fn ops_13_the_workflows_install_the_justfile_s_version_of_just() {
        let findings = findings_with(
            ".github/workflows/ci.yml",
            &format!(
                "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: taiki-e/install-action@{SHA}\n        with:\n          tool: just@1.58.0\n"
            ),
        );

        assert_eq!(
            findings,
            [
                ".github/workflows/ci.yml:7: just@1.58.0 differs from the justfile's just_version (OPS-13)"
            ]
        );
    }

    #[test]
    fn ops_13_an_image_is_pinned_by_digest() {
        let compose = findings_with(
            COMPOSE,
            "services:\n  postgres:\n    image: postgres:18.6\n",
        );
        let dockerfile = findings_with(
            DOCKERFILE,
            &format!(
                "# syntax=docker/dockerfile:1\nFROM alpine:3.24 AS base\nFROM base\nFROM missing-$BUILDARCH\nFROM scratch@sha256:{DIGEST}\n"
            ),
        );

        assert_eq!(
            compose,
            ["compose.yaml:3: the image postgres:18.6 is pinned by digest (OPS-13)"]
        );
        assert_eq!(
            dockerfile,
            [
                "Dockerfile:1: the frontend docker/dockerfile:1 is pinned by digest (OPS-13)",
                "Dockerfile:2: the image alpine:3.24 is pinned by digest (OPS-13)",
                "Dockerfile:4: the image missing-$BUILDARCH is pinned by digest (OPS-13)",
            ]
        );
    }

    #[test]
    fn ops_13_a_download_is_pinned_by_its_checksum_and_a_crate_by_its_version() {
        let findings = findings_with(
            DOCKERFILE,
            &format!(
                "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nADD https://example.org/zig.tar.xz /zig.tar.xz\nRUN cargo install cargo-chef@0.1 && true\nRUN cargo install --locked cargo-zigbuild\n"
            ),
        );

        assert_eq!(
            findings,
            [
                "Dockerfile:2: a download is pinned by its checksum, with --checksum=sha256: (OPS-13)",
                "Dockerfile:3: cargo-chef@0.1 names an exact version, as name@1.2.3; cargo install takes --locked (OPS-13)",
                "Dockerfile:4: cargo-zigbuild names an exact version, as name@1.2.3 (OPS-13)",
            ]
        );
    }

    #[test]
    fn ops_13_the_justfile_pins_each_tool_to_an_exact_version() {
        let findings = findings_with(
            JUSTFILE,
            "just_version := \"1.59.0\"\nwatchexec_version := \"2\"\n",
        );

        assert_eq!(
            findings,
            ["justfile:2: watchexec_version is an exact version, such as 1.2.3 (OPS-13)"]
        );
    }

    #[test]
    fn code_01_the_toolchain_comes_from_rust_toolchain_toml() {
        let toolchain = findings_with(TOOLCHAIN, "[toolchain]\nchannel = \"stable\"\n");
        let workflow = findings_with(
            ".github/workflows/ci.yml",
            &format!(
                "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: dtolnay/rust-toolchain@{SHA}\n      - run: rustup default 1.98.0\n"
            ),
        );
        let dockerfile = findings_with(
            DOCKERFILE,
            &format!(
                "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nRUN rustup toolchain install 1.98.0\nRUN cargo +nightly build\n"
            ),
        );

        assert_eq!(
            toolchain,
            ["rust-toolchain.toml:1: the channel is an exact version, such as 1.99.1 (CODE-01)"]
        );
        assert_eq!(workflow.len(), 2, "{workflow:?}");
        assert!(workflow[0].contains("dtolnay/rust-toolchain installs a toolchain"));
        assert!(workflow[1].contains("installs or picks no toolchain"));
        assert_eq!(dockerfile.len(), 2, "{dockerfile:?}");
    }

    #[test]
    fn code_01_every_crate_takes_edition_2024_from_the_workspace() {
        let member = findings_with("crates/app/Cargo.toml", "[package]\nedition = \"2021\"\n");
        let workspace = findings_with(WORKSPACE, "[workspace.package]\nedition = \"2021\"\n");

        assert_eq!(
            member,
            ["crates/app/Cargo.toml:1: a crate takes its edition from the workspace (CODE-01)"]
        );
        assert_eq!(
            workspace,
            ["Cargo.toml:1: the workspace's edition is 2024 (CODE-01)"]
        );
    }
}
