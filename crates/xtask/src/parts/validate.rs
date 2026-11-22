//! What `just remove`, `just keep` and `check-parts` refuse before they remove anything: a marker
//! that names no part, a line that holds two markers, a block that no end marker closes, a test
//! that removing a part would take though its behavior stays, unless the test proves a clause
//! that exists only with another part too (ARCH-07, TEST-04), and a row of the security map that
//! cites in the cells that a removal keeps of it (`asvs`).

use super::{Part, SPECS, markers, owned};
use crate::{Finding, repo::Repo};

/// Fails on each marker of the repository's files that names no part, each line that holds two
/// markers, each block that no end marker closes, and each test, in a file that a part owns
/// whole, that another module's behavior names, since the removal would take it, unless that
/// behavior's row in its spec is the part's too, and goes with it, or the row is another part's
/// whose marker marks the line that declares the test, which then proves a clause that exists
/// only with both parts, and goes with either (ARCH-07). For a part without a module of its own,
/// such as an OAuth provider, every module is another.
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
    findings.extend(super::asvs::citing_cells(repo));
    for part in parts {
        let module = part.module.as_deref();
        let owned = owned(repo, part);
        for file in repo
            .files()
            .filter(|file| file.path.ends_with(".rs") && owned.contains(&file.path))
        {
            // The parts that mark each line of the file; a file whose markers don't read is
            // refused above.
            let marking = markers::owners(&file.text, &known).unwrap_or_default();
            for (index, line) in file.text.lines().enumerate() {
                let Some(name) = line
                    .trim_start()
                    .strip_prefix("async fn ")
                    .or_else(|| line.trim_start().strip_prefix("fn "))
                else {
                    continue;
                };
                let Some((other, number)) =
                    behavior(name).filter(|(other, _)| Some(other.as_str()) != module)
                else {
                    continue;
                };
                let row = row_owners(repo, &known, &other, &number);
                let test = marking.get(index).map_or(&[][..], Vec::as_slice);
                if !(row.contains(&part.name.as_str()) || row.iter().any(|by| test.contains(by))) {
                    findings.push(Finding::new(
                        &file.path,
                        index + 1,
                        format!(
                            "a test of `{other}`'s behavior is in a file that `{}` owns, so \
                             removing the part would remove it while the behavior stays: move \
                             it, prove the behavior without the part, give the behavior's row \
                             the part's marker if it exists only for the part, or, for a clause \
                             that exists only with the part whose marker the row carries, put \
                             the test in that part's block (TEST-04)",
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

/// The parts whose markers own the row of `module`'s behavior `number` in that module's spec: the
/// row carries the part's marker, or the part's block holds it. None where the spec, or the row,
/// isn't there, or the spec's markers don't read.
fn row_owners<'a>(repo: &'a Repo, known: &[&str], module: &str, number: &str) -> Vec<&'a str> {
    let Some(spec) = repo.file(&format!("{SPECS}{module}.md")) else {
        return Vec::new();
    };
    let row = format!("| {}-{number} |", module.to_ascii_uppercase());
    let Ok(owners) = markers::owners(&spec.text, known) else {
        return Vec::new();
    };
    spec.text
        .lines()
        .zip(owners)
        .filter(|(line, _)| line.starts_with(&row))
        .flat_map(|(_, owners)| owners)
        .collect()
}
