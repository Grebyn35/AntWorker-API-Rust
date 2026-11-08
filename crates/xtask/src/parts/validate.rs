//! What `just remove` and `check-parts` refuse before they remove anything: a marker that names
//! no part, a block that no end marker closes, and a test that removing a part would take though
//! its behavior stays (ARCH-07, TEST-04).

use super::{Part, SPECS, markers, owned};
use crate::{Finding, repo::Repo};

/// Fails on each marker of the repository's files that names no part, each block that no end
/// marker closes, and each test, in a file that a part owns whole, that another module's behavior
/// names, since the removal would take it, unless that behavior's row in its spec is the part's
/// too, and goes with it (ARCH-07).
pub(super) fn validate(repo: &Repo, parts: &[Part]) -> Vec<Finding> {
    let known: Vec<&str> = parts.iter().map(|part| part.name.as_str()).collect();
    let mut findings: Vec<Finding> = repo
        .files()
        .filter_map(|file| {
            markers::owners(&file.text, &known)
                .err()
                .map(|(line, message)| Finding::new(&file.path, line, message))
        })
        .collect();
    for part in parts {
        let Some(module) = &part.module else {
            continue;
        };
        let owned = owned(repo, part);
        for file in repo
            .files()
            .filter(|file| file.path.ends_with(".rs") && owned.contains(&file.path))
        {
            for (index, line) in file.text.lines().enumerate() {
                let Some(name) = line
                    .trim_start()
                    .strip_prefix("async fn ")
                    .or_else(|| line.trim_start().strip_prefix("fn "))
                else {
                    continue;
                };
                let Some((other, number)) = behavior(name).filter(|(other, _)| other != module)
                else {
                    continue;
                };
                if !removes_behavior(repo, part, &known, &other, &number) {
                    findings.push(Finding::new(
                        &file.path,
                        index + 1,
                        format!(
                            "a test of `{other}`'s behavior is in a file that `{}` owns, so \
                             removing the part would remove it while the behavior stays: move \
                             it, prove the behavior without the part, or give the behavior's row \
                             the part's marker if it exists only for the part (TEST-04)",
                            part.name
                        ),
                    ));
                }
            }
        }
    }
    findings
}

/// The module and the number of the behavior that the function `name` is named after, as
/// `workspaces` and `005` for `workspaces_005_...`: the words before its three-digit number, and
/// the number.
pub(super) fn behavior(name: &str) -> Option<(String, String)> {
    let name: String = name
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let words: Vec<&str> = name.split('_').collect();
    let number = words
        .iter()
        .position(|word| word.len() == 3 && word.chars().all(|c| c.is_ascii_digit()))?;
    (number > 0 && number + 1 < words.len())
        .then(|| (words[..number].join("_"), words[number].to_owned()))
}

/// Whether removing `part` removes the row of `module`'s behavior `number` from that module's
/// spec, because the row is the part's: it carries the part's marker, or the part's block holds
/// it.
fn removes_behavior(repo: &Repo, part: &Part, known: &[&str], module: &str, number: &str) -> bool {
    let Some(spec) = repo.file(&format!("{SPECS}{module}.md")) else {
        return false;
    };
    let row = format!("| {}-{number} |", module.to_ascii_uppercase());
    let Ok(owners) = markers::owners(&spec.text, known) else {
        return false;
    };
    spec.text
        .lines()
        .zip(owners)
        .any(|(line, owners)| line.starts_with(&row) && owners.contains(&part.name.as_str()))
}
