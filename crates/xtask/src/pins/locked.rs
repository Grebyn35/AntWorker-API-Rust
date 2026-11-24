//! The pin that `Cargo.lock` sets: sqlx-cli writes the query cache that the locked sqlx reads
//! (DATA-14), so the justfile pins sqlx-cli at that sqlx's version (OPS-11), and a pull request
//! that moves sqlx fails until the pin moves with it.

use crate::{Finding, repo::Repo};

/// The file that holds the locked versions.
const LOCK: &str = "Cargo.lock";

/// A finding when a line `version = "<version>"` that follows a line `name = "sqlx"` of
/// `Cargo.lock` names another version than the justfile's line `sqlx_cli_version :=
/// "<version>"`, or when no such line follows one. A justfile without the line pins nothing to
/// compare.
pub(super) fn sqlx_cli(repo: &Repo, findings: &mut Vec<Finding>) {
    let text = |path: &str| repo.file(path).map_or("", |file| file.text.as_str());
    let pinned = text(super::JUSTFILE)
        .lines()
        .enumerate()
        .find_map(|(index, line)| {
            let (name, value) = line.split_once(":=")?;
            (name.trim() == "sqlx_cli_version").then(|| (index + 1, value.trim().trim_matches('"')))
        });
    let Some((line, pin)) = pinned else {
        return;
    };
    let lock: Vec<&str> = text(LOCK).lines().collect();
    let locked: Vec<&str> = lock
        .windows(2)
        .filter(|pair| pair[0] == "name = \"sqlx\"")
        .filter_map(|pair| pair[1].strip_prefix("version = \"")?.strip_suffix('"'))
        .collect();
    if locked.is_empty() || locked.iter().any(|version| *version != pin) {
        let holds = if locked.is_empty() {
            "no version of sqlx".to_owned()
        } else {
            format!("sqlx {}", locked.join(" and "))
        };
        findings.push(Finding::new(
            super::JUSTFILE,
            line,
            format!(
                "sqlx_cli_version is {pin}, and {LOCK} holds {holds}: sqlx-cli writes the query \
                 cache that the locked sqlx reads, so it takes that version (OPS-11)"
            ),
        ));
    }
}
