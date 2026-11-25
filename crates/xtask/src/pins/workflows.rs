//! What `check-pins` reads of a workflow: its YAML, as the events of serde-saphyr's parser give
//! it, and in it the value of each mapping key of [`KEYS`], wherever it stands, the scalar of a
//! `container` at [`JOB_KEYS`], and each key and scalar, which [`Reader::toolchain`] reads.

use serde_saphyr::granit_parser::{Event, Parser, StrInput};

use super::{
    commands::names_cargo_toolchain,
    forms::{has_digest, is_versioned_runner},
};
use crate::Finding;

/// The keys whose values name a pin.
const KEYS: [&str; 5] = ["uses", "runs-on", "runner", "image", "tool"];

/// The depth, from the document's root, of the mapping that holds a job's keys, such as its
/// `container`: the root's entries are at 0, and those of `jobs:`, the jobs, at 1.
const JOB_KEYS: usize = 2;

/// The `runs-on:` of a job that runs on a runner for each value of its matrix.
const MATRIX_RUNNER: &str = "${{ matrix.runner }}";

/// A node of a workflow's YAML, with the line it starts on.
struct Node {
    line: usize,
    kind: Kind,
}

enum Kind {
    /// A scalar's text, as the parser gives it: a quoted one without its quotes.
    Scalar(String),
    Sequence(Vec<Node>),
    Mapping(Vec<(Node, Node)>),
    /// An alias of an anchor, which the check doesn't follow.
    Alias,
}

/// The next node of `events`, or `None` at the end of a sequence, a mapping or the stream.
fn node<'a>(events: &mut Parser<'a, StrInput<'a>>) -> Result<Option<Node>, String> {
    while let Some(next) = events.next() {
        let (event, span) = next.map_err(|error| error.to_string())?;
        let kind = match event {
            Event::Scalar(text, ..) => Kind::Scalar(text.into_owned()),
            Event::Alias(_) | Event::UnresolvedAlias(_) => Kind::Alias,
            Event::SequenceStart(..) => {
                let mut items = Vec::new();
                while let Some(item) = node(events)? {
                    items.push(item);
                }
                Kind::Sequence(items)
            }
            Event::MappingStart(..) => {
                let mut entries = Vec::new();
                while let Some(key) = node(events)? {
                    let value = node(events)?.ok_or("a mapping's key has no value")?;
                    entries.push((key, value));
                }
                Kind::Mapping(entries)
            }
            Event::SequenceEnd | Event::MappingEnd | Event::StreamEnd => return Ok(None),
            _ => continue,
        };
        return Ok(Some(Node {
            line: span.start.line(),
            kind,
        }));
    }
    Ok(None)
}

/// Checks the workflow at `path`, whose YAML is `text`, against the justfile's `just_version`.
pub(super) fn workflow_pins(
    path: &str,
    text: &str,
    just_version: Option<&str>,
    findings: &mut Vec<Finding>,
) {
    let mut events = Parser::new_from_str(text);
    let mut reader = Reader {
        path,
        just_version,
        findings,
    };
    loop {
        match node(&mut events) {
            Ok(Some(root)) => {
                let mut job = Job::default();
                reader.node(&root, 0, &mut job);
                reader.findings.extend(job.finding(path));
            }
            Ok(None) => return,
            Err(error) => {
                reader
                    .findings
                    .push(Finding::new(path, 1, format!("not YAML: {error}")));
                return;
            }
        }
    }
}

/// What `check-pins` reads of the workflow's job it is in: the line of its `runs-on:` that names
/// its matrix's runner, if it has one, and whether its matrix names a `runner:`.
#[derive(Default)]
struct Job {
    matrix_runner: Option<usize>,
    runners: bool,
}

impl Job {
    /// The finding of a job that runs on its matrix's runner, while its matrix names none.
    fn finding(&self, path: &str) -> Option<Finding> {
        let number = self.matrix_runner.filter(|_| !self.runners)?;
        Some(Finding::new(
            path,
            number,
            "the job's matrix names each runner as `runner:`, a versioned label (OPS-11)",
        ))
    }
}

/// The reader of one workflow's nodes, with the findings it adds to.
struct Reader<'a> {
    path: &'a str,
    just_version: Option<&'a str>,
    findings: &'a mut Vec<Finding>,
}

impl Reader<'_> {
    fn push(&mut self, line: usize, problem: impl Into<String>) {
        self.findings.push(Finding::new(self.path, line, problem));
    }

    /// Reads `node`, at `depth` from the document's root, in `job`: each entry of a mapping at
    /// depth 1, such as a job under `jobs:`, is a job of its own.
    fn node(&mut self, node: &Node, depth: usize, job: &mut Job) {
        match &node.kind {
            Kind::Scalar(text) => self.toolchain(node.line, text),
            Kind::Alias => self.push(
                node.line,
                "check-pins doesn't read a YAML alias in a workflow: write the value out (OPS-11)",
            ),
            Kind::Sequence(items) => {
                for item in items {
                    self.node(item, depth + 1, job);
                }
            }
            Kind::Mapping(entries) => {
                for (key, value) in entries {
                    if depth == 1 {
                        let mut own = Job::default();
                        self.entry(key, value, depth, &mut own);
                        self.findings.extend(own.finding(self.path));
                    } else {
                        self.entry(key, value, depth, job);
                    }
                }
            }
        }
    }

    /// Reads the entry of `key` and `value`: a key of [`KEYS`] in another case is refused.
    fn entry(&mut self, key: &Node, value: &Node, depth: usize, job: &mut Job) {
        self.node(key, depth + 1, job);
        if let Kind::Scalar(name) = &key.kind {
            let lower = name.to_ascii_lowercase();
            if *name != lower && KEYS.contains(&lower.as_str()) {
                self.push(
                    key.line,
                    format!("check-pins reads `{lower}:` in lower case (OPS-11)"),
                );
            }
            self.pin(name, value, job);
            if depth == JOB_KEYS && name == "container" {
                self.container(value);
            }
        }
        self.node(value, depth + 1, job);
    }

    /// Checks the image that a job's `container:` names as a scalar; one that it names in a
    /// mapping stands under `image:`, which [`Reader::pin`] reads.
    fn container(&mut self, value: &Node) {
        if let Kind::Scalar(text) = &value.kind
            && !text.is_empty()
            && !has_digest(text)
        {
            self.push(
                value.line,
                format!("the image {text} is pinned by digest (OPS-11)"),
            );
        }
    }

    /// Checks the pin that the value of the key `key` names, if any.
    fn pin(&mut self, key: &str, value: &Node, job: &mut Job) {
        let line = value.line;
        let text = match &value.kind {
            Kind::Scalar(text) => text.as_str(),
            Kind::Alias => return,
            Kind::Sequence(_) | Kind::Mapping(_) => {
                if matches!(key, "uses" | "runs-on" | "runner") {
                    self.push(
                        line,
                        format!(
                            "`{key}:` holds a list or a mapping, which check-pins doesn't read: \
                             write one value (OPS-11)"
                        ),
                    );
                }
                return;
            }
        };
        match key {
            "uses" => {
                if let Some(problem) = action_problem(text) {
                    self.push(line, problem);
                }
            }
            "runs-on" | "runner" => self.runner(key, text, line, job),
            // An empty value names no image, nor, above, does a list or a mapping, such as a job
            // named `image`.
            "image" if !text.is_empty() && !has_digest(text) => {
                self.push(
                    line,
                    format!("the image {text} is pinned by digest (OPS-11)"),
                );
            }
            "tool" => self.tools(text, line),
            _ => {}
        }
    }

    /// Checks the runner that `runs-on:` or a matrix's `runner:` names, at `line`.
    fn runner(&mut self, key: &str, runner: &str, line: usize, job: &mut Job) {
        // A job that runs on a runner for each value of its matrix names it from the matrix,
        // whose `runner:` values are checked as labels.
        if runner == MATRIX_RUNNER {
            job.matrix_runner = Some(line);
            return;
        }
        if key == "runner" {
            job.runners = true;
        }
        if !is_versioned_runner(runner) {
            self.push(
                line,
                format!("the runner {runner} is a versioned label, such as ubuntu-24.04 (OPS-11)"),
            );
        }
    }

    /// Checks each tool of the list `tools`, split at commas and whitespace, that is `just` or
    /// starts with `just@`.
    fn tools(&mut self, tools: &str, line: usize) {
        for tool in tools.split(|c: char| c == ',' || c.is_whitespace()) {
            let problem = match tool.strip_prefix("just@") {
                _ if tool == "just" => format!(
                    "just names no version: the workflows install the justfile's just_version, \
                     as just@{} (OPS-11)",
                    self.just_version.unwrap_or("<version>")
                ),
                Some(version) if Some(version) != self.just_version => {
                    format!("just@{version} differs from the justfile's just_version (OPS-11)")
                }
                _ => continue,
            };
            self.push(line, problem);
        }
    }

    /// Refuses the key or the scalar `text` at `line` if it holds `rustup`, in any case, or if
    /// [`names_cargo_toolchain`] reads it.
    fn toolchain(&mut self, line: usize, text: &str) {
        let words: Vec<&str> = text.split_whitespace().collect();
        if text.to_ascii_lowercase().contains("rustup") || names_cargo_toolchain(text, &words) {
            self.push(
                line,
                "a workflow installs or picks no toolchain: `just setup` installs the one \
                 rust-toolchain.toml pins (CODE-01)",
            );
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
            .then(|| format!("the image {image} is pinned by digest (OPS-11)"));
    }
    let pinned = action
        .rsplit_once('@')
        .is_some_and(|(_, sha)| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()));
    if !pinned {
        return Some(format!(
            "the action {action} is pinned by the SHA of a commit (OPS-11)"
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
