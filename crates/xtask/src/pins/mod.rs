//! `check-pins`: what a build or a check uses is pinned as docs/operations/ci.md's rows of the
//! check say (OPS-11), and the toolchain and the edition are the ones `rust-toolchain.toml` and
//! the workspace name (CODE-01).

mod commands;
mod forms;
mod instructions;
mod locked;
mod workflows;

use commands::{bare, command_problems, picks_toolchain};
use forms::{has_digest, is_deploy_variable, is_exact_version};
use instructions::{escape_directives, instructions};
use workflows::workflow_pins;

use crate::{Finding, Outcome, repo::Repo, shell};

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
    justfiles_toolchain(repo, &mut findings);
    locked::sqlx_cli(repo, &mut findings);
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
                format!("{name} is an exact version, such as 1.2.3 (OPS-11)"),
            ));
        }
        if name == "just_version" {
            just = Some(version.to_owned());
        }
    }
    just
}

/// Refuses each line of the justfile, and of each file that its `import` lines name, that
/// [`picks_toolchain`] reads, the lines read as [`instructions()`] reads a Dockerfile's, each
/// without the `@` and `-` that start it.
fn justfiles_toolchain(repo: &Repo, findings: &mut Vec<Finding>) {
    for file in shell::justfiles(repo) {
        for (number, line) in instructions(&file.text) {
            let line = line.trim_start_matches(['@', '-']);
            let words: Vec<&str> = line.split_whitespace().collect();
            if picks_toolchain(line, &words) {
                findings.push(Finding::new(
                    &file.path,
                    number,
                    "the tasks build with the toolchain rust-toolchain.toml pins (CODE-01)",
                ));
            }
        }
    }
}

/// Checks the images that the compose file at `path` names. An image that
/// [`is_deploy_variable`] reads passes as a variable that the deploy sets to a digest.
fn compose_pins(path: &str, text: &str, findings: &mut Vec<Finding>) {
    for (number, line) in numbered(text) {
        if let Some(image) = value_of(line.trim_start(), "image:")
            && !has_digest(image)
            && !is_deploy_variable(image)
        {
            findings.push(Finding::new(
                path,
                number,
                format!("the image {image} is pinned by digest (OPS-11)"),
            ));
        }
    }
}

/// Checks the frontend, the parser directives, the images, the downloads, the packages, the
/// crates and the toolchain of the Dockerfile at `path`, each instruction whole, at the line it
/// starts on.
fn dockerfile_pins(path: &str, text: &str, findings: &mut Vec<Finding>) {
    let mut stages: Vec<String> = Vec::new();
    let first = text.lines().next().unwrap_or_default().trim();
    let frontend = first.strip_prefix("# syntax=");
    if !frontend.is_some_and(has_digest) {
        let missing = "the first line pins the Dockerfile frontend, with `# syntax=` and a digest";
        let problem = frontend.map_or(missing.to_owned(), |frontend| {
            format!("the frontend {frontend} is pinned by digest")
        });
        findings.push(Finding::new(path, 1, format!("{problem} (OPS-11)")));
    }
    for number in escape_directives(text) {
        findings.push(Finding::new(
            path,
            number,
            "check-pins reads a Dockerfile whose lines go on after a `\\`: remove the parser \
             directive escape (OPS-11)",
        ));
    }
    for (number, line) in &instructions(text) {
        let (number, line) = (*number, line.as_str());
        let words: Vec<&str> = line.split_whitespace().collect();
        let mut problems = match words
            .first()
            .map(|word| word.to_ascii_uppercase())
            .as_deref()
        {
            Some("FROM") => {
                let mut rest = words[1..].iter().skip_while(|word| word.starts_with("--"));
                let image = rest.next().copied().unwrap_or_default();
                let problem = image_problem(image, &stages);
                if let (Some(as_), Some(stage)) = (rest.next(), rest.next())
                    && as_.eq_ignore_ascii_case("as")
                {
                    stages.push((*stage).to_owned());
                }
                problem.into_iter().collect()
            }
            Some("COPY" | "RUN") => option_problems(&words, &stages),
            Some("ADD") => add_problem(&words).into_iter().collect(),
            _ => Vec::new(),
        };
        problems.extend(command_problems(line, &words));
        for problem in problems {
            findings.push(Finding::new(path, number, problem));
        }
    }
}

/// What is wrong with `image`, if it names neither its digest nor one of `stages`.
fn image_problem(image: &str, stages: &[String]) -> Option<String> {
    (!has_digest(image) && !names_stage(image, stages))
        .then(|| format!("the image {image} is pinned by digest (OPS-11)"))
}

/// What is wrong with the options of the `COPY` or `RUN` split into `words`, the words that
/// start with `--` before its arguments: the image that a `--from=` names, or the key `from`, in
/// any case, of a `--mount=`, unless it names a stage or is a whole number; and an option
/// whose name, before any `=`, is `--from` or `--mount` in any case, unless it starts with
/// `--from=` or `--mount=` and its value holds no quote.
fn option_problems(words: &[&str], stages: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    for option in words[1..].iter().take_while(|word| word.starts_with("--")) {
        let (name, value) = option.split_once('=').unwrap_or((option, ""));
        let lower = name.to_ascii_lowercase();
        if lower != "--from" && lower != "--mount" {
            continue;
        }
        if name != lower || !option.contains('=') || value.contains(['"', '\'']) {
            problems.push(
                "check-pins reads a --from= or a --mount= in lower case, with no quote in its \
                 value (OPS-11)"
                    .to_owned(),
            );
            continue;
        }
        let images: Vec<&str> = if name == "--from" {
            vec![value]
        } else {
            value
                .split(',')
                .filter_map(|part| {
                    let (key, image) = part.split_once('=')?;
                    key.eq_ignore_ascii_case("from").then_some(image)
                })
                .collect()
        };
        problems.extend(
            images
                .into_iter()
                .filter(|image| image.parse::<usize>().is_err())
                .filter_map(|image| image_problem(image, stages)),
        );
    }
    problems
}

/// What is wrong with the `ADD` split into `words`, if anything: one of its words whose
/// [`bare`] form starts with `http://` or `https://`, in any case, or one of its words after
/// its options, those that start with `--`, that holds a `$`, while none of its words starts
/// with `--checksum=sha256:`.
fn add_problem(words: &[&str]) -> Option<String> {
    if words
        .iter()
        .any(|word| word.starts_with("--checksum=sha256:"))
    {
        return None;
    }
    let remote = words.iter().any(|word| {
        let word = bare(word).to_ascii_lowercase();
        word.starts_with("https://") || word.starts_with("http://")
    });
    if remote {
        return Some(
            "a download is pinned by its checksum, with --checksum=sha256: (OPS-11)".to_owned(),
        );
    }
    let variable = words[1..]
        .iter()
        .skip_while(|word| word.starts_with("--"))
        .any(|word| word.contains('$'));
    variable.then(|| {
        "check-pins doesn't read a variable in an ADD: pin it by its checksum, with \
         --checksum=sha256:, or write its source out (OPS-11)"
            .to_owned()
    })
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
    fn ops_11_every_dockerfile_and_compose_file_is_checked() {
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
                "deploy/postgres/Dockerfile:1: the frontend docker/dockerfile:1 is pinned by digest (OPS-11)",
                "deploy/postgres/Dockerfile:2: the image postgres:18.6 is pinned by digest (OPS-11)",
            ]
        );
        assert_eq!(
            compose,
            ["deploy/compose.yaml:3: the image caddy:2.11.7 is pinned by digest (OPS-11)"]
        );
    }

    #[test]
    fn ops_11_an_image_that_a_copy_or_a_mount_names_is_pinned_by_digest() {
        let dockerfile = format!(
            "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nFROM alpine:3.24.3@sha256:{DIGEST} AS base\n\
             COPY --from=base /a /a\nCOPY --from=0 /b /b\nCOPY --from=busybox:1.37@sha256:{DIGEST} /c /c\n\
             COPY --from=busybox:1.37 /d /d\nRUN --mount=type=cache,from=base,target=/e true\n\
             RUN --mount=type=bind,from=golang:1.26,target=/f true\nRUN tool --from=anything\n"
        );

        let findings = findings_with(DOCKERFILE, &dockerfile);

        assert_eq!(
            findings,
            [
                "Dockerfile:6: the image busybox:1.37 is pinned by digest (OPS-11)",
                "Dockerfile:8: the image golang:1.26 is pinned by digest (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_an_image_that_a_mount_s_first_key_names_is_pinned_by_digest() {
        // A mount of the default type, bind, whose first key is `from=`.
        let dockerfile = format!(
            "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nFROM alpine:3.24.3@sha256:{DIGEST} AS base\n\
             RUN --mount=from=base,target=/a true\n\
             RUN --mount=from=golang:1.26@sha256:{DIGEST},target=/b true\n\
             RUN --mount=from=golang:1.26,target=/c true\n"
        );

        let findings = findings_with(DOCKERFILE, &dockerfile);

        assert_eq!(
            findings,
            ["Dockerfile:5: the image golang:1.26 is pinned by digest (OPS-11)"]
        );
    }

    #[test]
    fn ops_11_a_later_stage_a_stage_of_one_platform_and_a_number_after_from_are_images() {
        let dockerfile = format!(
            "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\n\
             COPY --from=later /a /a\nFROM alpine:3.24.3@sha256:{DIGEST} AS later\n\
             FROM alpine:3.24.3@sha256:{DIGEST} AS zig-amd64\nCOPY --from=zig-$BUILDARCH /b /b\n\
             FROM 0\n"
        );

        let findings = findings_with(DOCKERFILE, &dockerfile);

        assert_eq!(
            findings,
            [
                "Dockerfile:2: the image later is pinned by digest (OPS-11)",
                "Dockerfile:5: the image zig-$BUILDARCH is pinned by digest (OPS-11)",
                "Dockerfile:6: the image 0 is pinned by digest (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_a_compose_image_from_a_variable_the_deploy_sets_passes_and_one_with_a_default_fails()
    {
        let findings = findings_with(
            "deploy/compose.yaml",
            "services:\n  a:\n    image: ${API_IMAGE_BLUE:?the image, from the deploy}\n  b:\n    image: ${API_IMAGE_NEW}\n  c:\n    image: ${API_IMAGE_GREEN:-postgres:18}\n  d:\n    image: ${POSTGRES_IMAGE?the image}\n",
        );

        assert_eq!(
            findings,
            [
                "deploy/compose.yaml:7: the image ${API_IMAGE_GREEN:-postgres:18} is pinned by digest (OPS-11)"
            ]
        );
    }

    #[test]
    fn ops_11_a_package_the_image_installs_is_pinned_by_version() {
        let findings = findings_with(
            "deploy/postgres/Dockerfile",
            &format!(
                "# syntax=docker/dockerfile:1@sha256:{DIGEST}\nFROM postgres:18.6@sha256:{DIGEST}\nRUN apt-get update && apt-get install --yes pgbackrest=2.59.3-1 curl && rm -rf /var/lib/apt/lists/*\n"
            ),
        );

        assert_eq!(
            findings,
            [
                "deploy/postgres/Dockerfile:3: the package curl is pinned by version, as curl=<version> (OPS-11)"
            ]
        );
    }

    #[test]
    fn ops_11_a_package_after_an_option_s_own_value_is_pinned_by_version() {
        let findings = findings_with(
            "deploy/postgres/Dockerfile",
            &format!(
                "# syntax=docker/dockerfile:1@sha256:{DIGEST}\nFROM postgres:18.6@sha256:{DIGEST}\nRUN apt-get -o Acquire::Retries=3 install curl && apt -t stable install jq && apt-get update\n"
            ),
        );

        assert_eq!(
            findings,
            [
                "deploy/postgres/Dockerfile:3: the package curl is pinned by version, as curl=<version> (OPS-11)",
                "deploy/postgres/Dockerfile:3: the package jq is pinned by version, as jq=<version> (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_a_package_or_a_crate_on_a_continuation_line_is_pinned_by_version() {
        let findings = findings_with(
            "deploy/postgres/Dockerfile",
            &format!(
                "# syntax=docker/dockerfile:1@sha256:{DIGEST}\nFROM postgres:18.6@sha256:{DIGEST}\nRUN apt-get update \\\n    && apt-get install --yes pgbackrest=2.59.3-1 \\\n        jq=1.7.1-6 curl \\\n    # a comment between continuation lines\n        wget \\\n    && apt-get -y install less \\\n    && apt install vim \\\n    && cargo install --locked \\\n        cargo-chef@0.1.78 cargo-zigbuild\n"
            ),
        );

        assert_eq!(
            findings,
            [
                "deploy/postgres/Dockerfile:3: cargo-zigbuild names an exact version, as name@1.2.3 (OPS-11)",
                "deploy/postgres/Dockerfile:3: the package curl is pinned by version, as curl=<version> (OPS-11)",
                "deploy/postgres/Dockerfile:3: the package wget is pinned by version, as wget=<version> (OPS-11)",
                "deploy/postgres/Dockerfile:3: the package less is pinned by version, as less=<version> (OPS-11)",
                "deploy/postgres/Dockerfile:3: the package vim is pinned by version, as vim=<version> (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_a_package_or_a_crate_whose_command_has_a_path_or_quotes_is_pinned_by_version() {
        let findings = findings_with(
            "deploy/postgres/Dockerfile",
            &format!(
                "# syntax=docker/dockerfile:1@sha256:{DIGEST}\nFROM postgres:18.6@sha256:{DIGEST}\nRUN /usr/bin/apt-get install -y curl\nRUN \"apt\" install jq && \\apt-get \"install\" wget\nRUN /usr/local/cargo/bin/cargo install --locked cargo-chef\n"
            ),
        );

        assert_eq!(
            findings,
            [
                "deploy/postgres/Dockerfile:3: the package curl is pinned by version, as curl=<version> (OPS-11)",
                "deploy/postgres/Dockerfile:4: the package jq is pinned by version, as jq=<version> (OPS-11)",
                "deploy/postgres/Dockerfile:4: the package wget is pinned by version, as wget=<version> (OPS-11)",
                "deploy/postgres/Dockerfile:5: cargo-chef names an exact version, as name@1.2.3 (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_an_onbuild_instruction_is_refused() {
        let findings = findings_with(
            DOCKERFILE,
            &format!(
                "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nONBUILD ADD https://example.org/zig.tar.xz /zig.tar.xz\nonbuild RUN [\"apt-get\", \"install\", \"curl\"]\nONBUILD \\\n    COPY --from=alpine:3.24 / /\nRUN echo ONBUILD\n"
            ),
        );

        assert_eq!(
            findings,
            [
                "Dockerfile:2: check-pins doesn't read an ONBUILD's instruction, which another image's build runs: write it in that build's Dockerfile (OPS-11)",
                "Dockerfile:3: check-pins doesn't read an ONBUILD's instruction, which another image's build runs: write it in that build's Dockerfile (OPS-11)",
                "Dockerfile:4: check-pins doesn't read an ONBUILD's instruction, which another image's build runs: write it in that build's Dockerfile (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_a_comment_that_ends_in_a_backslash_ends_at_its_line() {
        let dockerfile = format!(
            "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\n# RUN apt-get install -y \\\nFROM alpine:3.24\n  # ADD a.tar.xz \\\nADD https://example.org/b.tar.xz /b.tar.xz\n"
        );

        let findings = findings_with(DOCKERFILE, &dockerfile);

        assert_eq!(
            findings,
            [
                "Dockerfile:3: the image alpine:3.24 is pinned by digest (OPS-11)",
                "Dockerfile:5: a download is pinned by its checksum, with --checksum=sha256: (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_the_first_line_pins_the_frontend() {
        let later = findings_with(
            DOCKERFILE,
            &format!("# A comment.\n# syntax=docker/dockerfile:1@sha256:{DIGEST}\n"),
        );

        assert_eq!(
            later,
            [
                "Dockerfile:1: the first line pins the Dockerfile frontend, with `# syntax=` and a digest (OPS-11)"
            ]
        );
    }

    #[test]
    fn ops_11_a_pinned_repository_passes() {
        let files = pinned();
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();

        assert_eq!(check(&Repo::of(&files)).findings, []);
    }

    #[test]
    fn ops_11_an_action_is_pinned_by_a_commit_s_sha() {
        let findings = findings_with(
            ".github/workflows/ci.yml",
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v7\n      - uses: ./local\n",
        );

        assert_eq!(
            findings,
            [
                ".github/workflows/ci.yml:5: the action actions/checkout@v7 is pinned by the SHA of a commit (OPS-11)"
            ]
        );
    }

    #[test]
    fn ops_11_an_image_a_workflow_names_is_pinned_by_digest() {
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
            [".github/workflows/ci.yml:6: the image postgres:18 is pinned by digest (OPS-11)"]
        );
    }

    #[test]
    fn ops_11_a_runner_is_a_versioned_label() {
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
    fn ops_11_a_runner_that_a_matrix_names_is_a_versioned_label() {
        let workflow = |arm: &str| {
            format!(
                "jobs:\n  a:\n    runs-on: ${{{{ matrix.runner }}}}\n    strategy:\n      matrix:\n        include:\n          - platform: linux/amd64\n            runner: ubuntu-24.04\n          - platform: linux/arm64\n            runner: {arm}\n"
            )
        };

        let versioned = findings_with(".github/workflows/ci.yml", &workflow("ubuntu-24.04-arm"));
        let unversioned = findings_with(".github/workflows/ci.yml", &workflow("ubuntu-latest"));

        assert_eq!(versioned, Vec::<String>::new());
        assert_eq!(unversioned.len(), 1, "{unversioned:?}");
        assert!(
            unversioned[0].contains("ubuntu-latest is a versioned label"),
            "{unversioned:?}"
        );
    }

    #[test]
    fn ops_11_a_job_that_runs_on_its_matrix_s_runner_names_one_in_its_matrix() {
        let workflow = "jobs:\n  named:\n    runs-on: ${{ matrix.runner }}\n    strategy:\n      matrix:\n        include:\n          - runner: ubuntu-24.04\n  unnamed:\n    runs-on: ${{ matrix.runner }}\n    strategy:\n      matrix:\n        platform: [linux/amd64]\n";

        let findings = findings_with(".github/workflows/ci.yml", workflow);

        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings[0].contains(":9: the job's matrix names each runner"),
            "{findings:?}"
        );
    }

    #[test]
    fn ops_11_the_image_that_a_job_s_container_names_as_a_scalar_is_pinned_by_digest() {
        let digest = "a".repeat(64);
        let findings = findings_with(
            ".github/workflows/ci.yml",
            &format!(
                "jobs:
  tagged:
    runs-on: ubuntu-24.04
    container: golang:1.26
  pinned:
    runs-on: ubuntu-24.04
    container: golang:1.26@sha256:{digest}
  mapped:
    runs-on: ubuntu-24.04
    container:
      image: node:24
    steps:
      - uses: actions/checkout@{sha}
        with:
          container: an-input-of-the-action
",
                sha = "b".repeat(40)
            ),
        );

        assert_eq!(
            findings,
            [
                ".github/workflows/ci.yml:4: the image golang:1.26 is pinned by digest (OPS-11)",
                ".github/workflows/ci.yml:11: the image node:24 is pinned by digest (OPS-11)"
            ]
        );
    }

    #[test]
    fn ops_11_the_workflows_install_the_justfile_s_version_of_just() {
        let findings = findings_with(
            ".github/workflows/ci.yml",
            &format!(
                "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: taiki-e/install-action@{SHA}\n        with:\n          tool: just@1.58.0\n      - uses: taiki-e/install-action@{SHA}\n        with:\n          tool: just\n      - uses: taiki-e/install-action@{SHA}\n        with:\n          tool: cargo-deny,just\n      - uses: taiki-e/install-action@{SHA}\n        with:\n          tool: cargo-deny just\n      - uses: taiki-e/install-action@{SHA}\n        with:\n          tool: just@1.59.0, cargo-deny\n"
            ),
        );

        assert_eq!(
            findings,
            [
                ".github/workflows/ci.yml:7: just@1.58.0 differs from the justfile's just_version (OPS-11)",
                ".github/workflows/ci.yml:10: just names no version: the workflows install the justfile's just_version, as just@1.59.0 (OPS-11)",
                ".github/workflows/ci.yml:13: just names no version: the workflows install the justfile's just_version, as just@1.59.0 (OPS-11)",
                ".github/workflows/ci.yml:16: just names no version: the workflows install the justfile's just_version, as just@1.59.0 (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_a_run_or_an_add_in_the_exec_form_is_refused() {
        let findings = findings_with(
            DOCKERFILE,
            &format!(
                "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nadd [\"https://example.org/zig.tar.xz\", \"/zig.tar.xz\"]\nRUN [\"apt-get\", \"install\", \"-y\", \"curl\"]\nRUN --mount=type=cache,target=/root/.cargo [ \"cargo\", \"install\", \"cargo-chef\" ]\nRUN [ \"$TARGETARCH\" = amd64 ] || true\n"
            ),
        );

        assert_eq!(
            findings,
            [
                "Dockerfile:2: a RUN or an ADD takes the shell form, which check-pins reads, not a JSON array (OPS-11)",
                "Dockerfile:3: a RUN or an ADD takes the shell form, which check-pins reads, not a JSON array (OPS-11)",
                "Dockerfile:4: a RUN or an ADD takes the shell form, which check-pins reads, not a JSON array (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_an_image_is_pinned_by_digest() {
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
            ["compose.yaml:3: the image postgres:18.6 is pinned by digest (OPS-11)"]
        );
        assert_eq!(
            dockerfile,
            [
                "Dockerfile:1: the frontend docker/dockerfile:1 is pinned by digest (OPS-11)",
                "Dockerfile:2: the image alpine:3.24 is pinned by digest (OPS-11)",
                "Dockerfile:4: the image missing-$BUILDARCH is pinned by digest (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_a_download_is_pinned_by_its_checksum_and_a_crate_by_its_version() {
        let findings = findings_with(
            DOCKERFILE,
            &format!(
                "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nADD https://example.org/zig.tar.xz /zig.tar.xz\nRUN cargo install cargo-chef@0.1 && true\nRUN cargo install --locked cargo-zigbuild\n"
            ),
        );

        assert_eq!(
            findings,
            [
                "Dockerfile:2: a download is pinned by its checksum, with --checksum=sha256: (OPS-11)",
                "Dockerfile:3: cargo-chef@0.1 names an exact version, as name@1.2.3; cargo install takes --locked (OPS-11)",
                "Dockerfile:4: cargo-zigbuild names an exact version, as name@1.2.3 (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_the_justfile_pins_each_tool_to_an_exact_version() {
        let findings = findings_with(
            JUSTFILE,
            "just_version := \"1.59.0\"\nwatchexec_version := \"2\"\n",
        );

        assert_eq!(
            findings,
            ["justfile:2: watchexec_version is an exact version, such as 1.2.3 (OPS-11)"]
        );
    }

    #[test]
    fn ops_11_the_sqlx_cli_pin_is_the_version_of_the_locked_sqlx() {
        let one = "[[package]]\nname = \"sqlx\"\nversion = \"0.9.1\"\n\n\
                   [[package]]\nname = \"sqlx-core\"\nversion = \"0.9.0\"\n";
        let two = format!("{one}\n[[package]]\nname = \"sqlx\"\nversion = \"0.9.0\"\n");
        let with = |pin: &str, lock: &str| {
            let mut files = pinned();
            files[0]
                .1
                .push_str(&format!("sqlx_cli_version := \"{pin}\"\n"));
            files.push(("Cargo.lock".to_owned(), lock.to_owned()));
            let files: Vec<(&str, &str)> = files
                .iter()
                .map(|(path, text)| (path.as_str(), text.as_str()))
                .collect();
            check(&Repo::of(&files))
                .findings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };

        assert_eq!(with("0.9.1", one), Vec::<String>::new());
        assert_eq!(
            with("0.9.0", one),
            [
                "justfile:3: sqlx_cli_version is 0.9.0, and Cargo.lock holds sqlx 0.9.1: sqlx-cli \
                 writes the query cache that the locked sqlx reads, so it takes that version \
                 (OPS-11)"
            ]
        );
        // A pin that one of two locked versions names, but not the other.
        assert_eq!(
            with("0.9.0", &two),
            [
                "justfile:3: sqlx_cli_version is 0.9.0, and Cargo.lock holds sqlx 0.9.1 and 0.9.0: \
                 sqlx-cli writes the query cache that the locked sqlx reads, so it takes that \
                 version (OPS-11)"
            ]
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
                "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nRUN rustup toolchain install 1.98.0\nRUN cargo +nightly build\nRUN /usr/local/cargo/bin/rustup default 1.98.0\nRUN \"cargo\" +nightly build\n"
            ),
        );

        assert_eq!(
            toolchain,
            ["rust-toolchain.toml:1: the channel is an exact version, such as 1.99.1 (CODE-01)"]
        );
        assert_eq!(workflow.len(), 2, "{workflow:?}");
        assert!(workflow[0].contains("dtolnay/rust-toolchain installs a toolchain"));
        assert!(workflow[1].contains("installs or picks no toolchain"));
        assert_eq!(dockerfile.len(), 4, "{dockerfile:?}");
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

    #[test]
    fn ops_11_a_workflow_is_read_as_yaml() {
        let workflow = format!(
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      -   uses: actions/checkout@v7\n      - \"uses\": actions/cache@v5\n      - {{uses: actions/setup-node@v6}}\n      - uses : actions/upload-artifact@v6\n      - uses: actions/checkout@{SHA}\n  b:\n    runs-on: ${{{{ matrix.runner }}}}\n    strategy:\n      matrix:\n        include:\n          - runner: ubuntu-24.04\n          -   runner: ubuntu-latest\n"
        );

        let findings = findings_with(".github/workflows/ci.yml", &workflow);

        assert_eq!(
            findings,
            [
                ".github/workflows/ci.yml:5: the action actions/checkout@v7 is pinned by the SHA of a commit (OPS-11)",
                ".github/workflows/ci.yml:6: the action actions/cache@v5 is pinned by the SHA of a commit (OPS-11)",
                ".github/workflows/ci.yml:7: the action actions/setup-node@v6 is pinned by the SHA of a commit (OPS-11)",
                ".github/workflows/ci.yml:8: the action actions/upload-artifact@v6 is pinned by the SHA of a commit (OPS-11)",
                ".github/workflows/ci.yml:16: the runner ubuntu-latest is a versioned label, such as ubuntu-24.04 (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_a_workflow_s_list_mapping_or_alias_that_the_check_doesn_t_read_and_text_that_isn_t_yaml_are_refused()
     {
        let workflow = format!(
            "jobs:\n  a:\n    runs-on: [ubuntu-24.04]\n    steps:\n      - uses: &checkout actions/checkout@{SHA}\n      - uses: *checkout\n      - uses: {{name: actions/cache}}\n"
        );

        let read = findings_with(".github/workflows/ci.yml", &workflow);
        let unread = findings_with(".github/workflows/ci.yml", "jobs: [\n");

        assert_eq!(
            read,
            [
                ".github/workflows/ci.yml:3: `runs-on:` holds a list or a mapping, which check-pins doesn't read: write one value (OPS-11)",
                ".github/workflows/ci.yml:6: check-pins doesn't read a YAML alias in a workflow: write the value out (OPS-11)",
                ".github/workflows/ci.yml:7: `uses:` holds a list or a mapping, which check-pins doesn't read: write one value (OPS-11)",
            ]
        );
        assert_eq!(unread.len(), 1, "{unread:?}");
        assert!(
            unread[0].starts_with(".github/workflows/ci.yml:1: not YAML: "),
            "{unread:?}"
        );
    }

    #[test]
    fn ops_11_a_workflow_s_key_of_a_pin_in_another_case_is_refused() {
        let workflow = format!(
            "jobs:\n  a:\n    Runs-On: ubuntu-latest\n    steps:\n      - Uses: actions/checkout@v7\n      - uses: actions/checkout@{SHA}\n"
        );

        let findings = findings_with(".github/workflows/ci.yml", &workflow);

        assert_eq!(
            findings,
            [
                ".github/workflows/ci.yml:3: check-pins reads `runs-on:` in lower case (OPS-11)",
                ".github/workflows/ci.yml:5: check-pins reads `uses:` in lower case (OPS-11)",
            ]
        );
    }

    #[test]
    fn code_01_a_workflow_names_rustup_in_no_case_and_cargo_with_no_toolchain() {
        let workflow = "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    env:\n      RUSTUP_TOOLCHAIN: 1.98.0\n      \"Rustup_Toolchain\": 1.98.0\n    steps:\n      - {run: cargo  +nightly build}\n      - run: cargo build\n";

        let findings = findings_with(".github/workflows/ci.yml", workflow);

        let refused = "a workflow installs or picks no toolchain: `just setup` installs the one rust-toolchain.toml pins (CODE-01)";
        assert_eq!(
            findings,
            [5, 6, 8].map(|line| format!(".github/workflows/ci.yml:{line}: {refused}"))
        );
    }

    #[test]
    fn code_01_a_dockerfile_picks_no_toolchain_through_rustup_s_variable_run_override_or_default() {
        let dockerfile = format!(
            "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nENV RUSTUP_TOOLCHAIN=1.98.0\nARG rustup_toolchain\nRUN rustup run 1.98.0 cargo build\nRUN rustup override set 1.98.0\nRUN rustup -q +stable run nightly cargo build\nRUN \"rustup\" --verbose default nightly\nRUN rustup 'run' nightly cargo build\nRUN rustup default && rustup show && rustup toolchain install || true\n"
        );

        let findings = findings_with(DOCKERFILE, &dockerfile);

        let refused = "the image builds with the toolchain rust-toolchain.toml pins (CODE-01)";
        assert_eq!(
            findings,
            [2, 3, 4, 5, 6, 7, 8].map(|line| format!("Dockerfile:{line}: {refused}"))
        );
    }

    #[test]
    fn code_01_the_justfile_and_the_files_it_imports_pick_no_toolchain() {
        let mut files = pinned();
        files[0].1.push_str(
            "import 'just/build.just'\nexport RUSTUP_TOOLCHAIN := \"nightly\"\n\ntest:\n    cargo +nightly test\n\nsetup:\n    rustup show active-toolchain > /dev/null || rustup toolchain install\n    for tool in rustup git docker; do command -v \"$tool\"; done\n    rustup show active-toolchain | Out-Null; if ($LASTEXITCODE -ne 0) { rustup toolchain install }\n",
        );
        files.push((
            "just/build.just".to_owned(),
            "build:\n    @rustup run nightly cargo build\n    -rustup \\\n        override set nightly\n    # rustup run nightly, a comment\n".to_owned(),
        ));
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();

        let findings: Vec<String> = check(&Repo::of(&files))
            .findings
            .iter()
            .map(ToString::to_string)
            .collect();

        let refused = "the tasks build with the toolchain rust-toolchain.toml pins (CODE-01)";
        assert_eq!(
            findings,
            [
                format!("justfile:4: {refused}"),
                format!("justfile:7: {refused}"),
                format!("just/build.just:2: {refused}"),
                format!("just/build.just:3: {refused}"),
            ]
        );
    }

    #[test]
    fn ops_11_an_add_of_a_variable_or_of_a_quoted_address_is_pinned_by_its_checksum() {
        let dockerfile = format!(
            "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nARG ZIG_URL=https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz\nADD $ZIG_URL /zig.tar.xz\nADD ${{ZIG_URL}} /zig.tar.xz\nADD \"https://example.org/a.tar.xz\" /a\nADD HTTPS://example.org/b.tar.xz /b\nADD --checksum=sha256:{DIGEST} $ZIG_URL /zig.tar.xz\nADD --chown=$USER local.tar.xz /c\n"
        );

        let findings = findings_with(DOCKERFILE, &dockerfile);

        let variable = "check-pins doesn't read a variable in an ADD: pin it by its checksum, with --checksum=sha256:, or write its source out (OPS-11)";
        let download = "a download is pinned by its checksum, with --checksum=sha256: (OPS-11)";
        assert_eq!(
            findings,
            [
                format!("Dockerfile:3: {variable}"),
                format!("Dockerfile:4: {variable}"),
                format!("Dockerfile:5: {download}"),
                format!("Dockerfile:6: {download}"),
            ]
        );
    }

    #[test]
    fn ops_11_a_from_or_a_mount_in_quotes_or_in_another_case_is_refused_and_a_mount_s_key_has_no_case()
     {
        let dockerfile = format!(
            "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nFROM alpine:3.24.3@sha256:{DIGEST} AS base\nRUN --mount=\"from=golang:1.26,target=/go\" true\nRUN --mount=type=bind,FROM=golang:1.26,target=/go true\nRUN --MOUNT=from=golang:1.26 true\nRUN --mount from=golang:1.26 true\nCOPY --from='busybox:1.37' /a /a\nCOPY --From=busybox:1.37 /b /b\nRUN --mount=type=cache,From=base,target=/c true\n"
        );

        let findings = findings_with(DOCKERFILE, &dockerfile);

        let unread = "check-pins reads a --from= or a --mount= in lower case, with no quote in its value (OPS-11)";
        assert_eq!(
            findings,
            [
                format!("Dockerfile:3: {unread}"),
                "Dockerfile:4: the image golang:1.26 is pinned by digest (OPS-11)".to_owned(),
                format!("Dockerfile:5: {unread}"),
                format!("Dockerfile:6: {unread}"),
                format!("Dockerfile:7: {unread}"),
                format!("Dockerfile:8: {unread}"),
            ]
        );
    }

    #[test]
    fn ops_11_a_crate_that_cargo_installs_after_cargo_s_own_options_is_pinned_by_version() {
        let dockerfile = format!(
            "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nRUN cargo -q install cargo-chef\nRUN cargo --color never install --locked cargo-zigbuild\nRUN cargo --locked install cargo-auditable@0.7.7\n"
        );

        let findings = findings_with(DOCKERFILE, &dockerfile);

        assert_eq!(
            findings,
            [
                "Dockerfile:2: cargo-chef names an exact version, as name@1.2.3; cargo install takes --locked (OPS-11)",
                "Dockerfile:3: cargo-zigbuild names an exact version, as name@1.2.3 (OPS-11)",
            ]
        );
    }

    #[test]
    fn ops_11_a_command_s_words_end_at_each_word_that_ends_a_command() {
        for end in ["&&", "||", "|", ";", "&", "\\", ")", "}"] {
            let dockerfile = format!(
                "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\nRUN apt-get install --yes jq=1.7.1-6 {end} true && cargo install --locked cargo-chef@0.1.78 {end} true && rustup toolchain install {end} true\n"
            );

            let findings = findings_with(DOCKERFILE, &dockerfile);

            assert_eq!(findings, Vec::<String>::new(), "{end}");
        }
    }

    #[test]
    fn ops_11_a_dockerfile_s_parser_directive_escape_is_refused() {
        let dockerfile = format!(
            "# syntax=docker/dockerfile:1.27.1@sha256:{DIGEST}\n# escape=`\nFROM alpine:3.24.3@sha256:{DIGEST}\nRUN apt-get install --yes jq=1.7.1-6`\n    curl\n#ESCAPE=`\n\t#  Escape =  `\n# escaped=no, and the escape character\n"
        );

        let findings = findings_with(DOCKERFILE, &dockerfile);

        let refused = "check-pins reads a Dockerfile whose lines go on after a `\\`: remove the parser directive escape (OPS-11)";
        assert_eq!(
            findings,
            [2, 6, 7].map(|line| format!("Dockerfile:{line}: {refused}"))
        );
    }

    #[test]
    fn ops_11_a_compose_image_from_a_variable_the_deploy_doesn_t_set_or_with_text_after_it_is_pinned_by_digest()
     {
        let images = [
            "${REGISTRY:?}/api:${TAG:?}",
            "${API_IMAGE_BLUE:?the image}${TAG}",
            "${OTHER}",
            "${TAG?the image}",
            "${api_image_blue}",
            "${API_IMAGE_BLUE_2:?the image}",
        ];
        let compose: String = std::iter::once("services:\n".to_owned())
            .chain(
                images
                    .iter()
                    .enumerate()
                    .map(|(at, image)| format!("  s{at}:\n    image: {image}\n")),
            )
            .collect();

        let findings = findings_with("deploy/compose.yaml", &compose);

        let expected: Vec<String> = images
            .iter()
            .enumerate()
            .map(|(at, image)| {
                format!(
                    "deploy/compose.yaml:{}: the image {image} is pinned by digest (OPS-11)",
                    2 * at + 3
                )
            })
            .collect();
        assert_eq!(findings, expected);
    }

    #[test]
    fn ops_11_the_deploy_writes_each_variable_that_a_compose_image_may_name() {
        let repo = Repo::load().unwrap();
        let apply = repo.file("deploy/tasks/apply.sh").unwrap();

        for name in forms::DEPLOY_IMAGES {
            assert!(
                apply.text.contains(&format!("printf '%s\\n' \"{name}=$")),
                "{name}"
            );
        }
    }
}
