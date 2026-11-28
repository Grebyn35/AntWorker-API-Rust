//! Two lists that documents keep of the code, which `check-docs` holds to it: the subcommands
//! of `xtask` in `docs/operations/ci.md`, and the Rust files directly in the composition root's
//! directory in the layout of `app` in `docs/architecture.md` (DOC-01).

use std::collections::BTreeSet;

use crate::{Finding, repo::Repo, text};

/// Each name that `just/check.just` runs as `{{ xtask }} <name>` is among the code spans of the
/// line of `docs/operations/ci.md` that says they are `xtask`'s subcommands, before those words,
/// and of each line right after it that starts `So are`, and each of those is one that it runs,
/// as ci.md asks of a new check.
pub fn subcommands(repo: &Repo) -> Vec<Finding> {
    let (Some(recipes), Some(ci)) = (
        repo.file("just/check.just"),
        repo.file("docs/operations/ci.md"),
    ) else {
        return Vec::new();
    };
    let run: BTreeSet<&str> = recipes
        .text
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("{{ xtask }} "))
        .filter_map(|command| command.split_whitespace().next())
        .collect();
    let lines: Vec<&str> = ci.text.lines().collect();
    let opening = " are subcommands of the `xtask` crate";
    let start = lines.iter().position(|line| line.contains(opening));
    let listed: BTreeSet<&str> = start
        .into_iter()
        .flat_map(|start| {
            let list = lines[start].split(opening).next().unwrap_or_default();
            let after = lines[start + 1..]
                .iter()
                .take_while(|line| line.starts_with("So are "));
            std::iter::once(list).chain(after.copied())
        })
        .flat_map(|line| line.split('`').skip(1).step_by(2))
        .collect();
    let path = "docs/operations/ci.md";
    let line = start.map_or(1, |start| start + 1);
    let unlisted = run.difference(&listed).map(|name| {
        format!("`{name}`, which just/check.just runs through xtask, isn't in the list of xtask's subcommands (DOC-01)")
    });
    let unrun = listed.difference(&run).map(|name| {
        format!("the list of xtask's subcommands names `{name}`, which just/check.just doesn't run through xtask (DOC-01)")
    });
    unlisted
        .chain(unrun)
        .map(|message| Finding::new(path, line, message))
        .collect()
}

/// Each Rust file directly in `crates/app/src/composition/` is named, as `<name>.rs`, on the line of
/// `docs/architecture.md` that starts with `composition/`, white space aside, or on a line right
/// after it that holds nothing in that name's columns, as the layout of `app` describes it (DOC-01).
pub fn composition(repo: &Repo) -> Vec<Finding> {
    const DIRECTORY: &str = "crates/app/src/composition/";
    let Some(architecture) = repo.file("docs/architecture.md") else {
        return Vec::new();
    };
    let lines: Vec<&str> = architecture.text.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|line| line.trim_start().starts_with("composition/"))
    else {
        return Vec::new();
    };
    let width = lines[start].len() - lines[start].trim_start().len() + "composition/".len();
    let entry: Vec<&str> = std::iter::once(lines[start])
        .chain(lines[start + 1..].iter().copied().take_while(|line| {
            line.get(..width).is_some_and(|head| head.trim().is_empty()) && line.len() > width
        }))
        .collect();
    repo.paths()
        .filter_map(|path| path.strip_prefix(DIRECTORY))
        .filter(|name| name.ends_with(".rs") && !name.contains('/'))
        .filter(|name| !entry.iter().any(|line| text::has_word(line, name)))
        .map(|name| {
            Finding::new(
                "docs/architecture.md",
                start + 1,
                format!("the layout of `app` doesn't name composition/{name} (DOC-01)"),
            )
        })
        .collect()
}
