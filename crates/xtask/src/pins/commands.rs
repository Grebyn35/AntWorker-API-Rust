//! What `check-pins` reads of the commands of a Dockerfile's instruction: the crates and the
//! packages that it installs, the toolchain that it picks, and the forms that the check doesn't
//! read.

use super::{forms::is_exact_version, instructions::is_exec_form};

/// The words that end a command's words, for each reader of a command below.
const ENDS: [&str; 8] = ["&&", "||", "|", ";", "&", "\\", ")", "}"];

/// What is wrong with the commands of the Dockerfile instruction `line`, split into `words`: a
/// crate or a package installed without its version, a toolchain that [`picks_toolchain`]
/// reads, a `RUN` or an `ADD` in the exec form, as [`is_exec_form`] reads it, or an instruction
/// whose first word is `ONBUILD`, in any case, which the check doesn't read.
pub(super) fn command_problems(line: &str, words: &[&str]) -> Vec<String> {
    let mut problems: Vec<String> = cargo_install_problems(words);
    if is_exec_form(line) {
        problems.push(
            "a RUN or an ADD takes the shell form, which check-pins reads, not a JSON array \
             (OPS-11)"
                .to_owned(),
        );
    }
    if words
        .first()
        .is_some_and(|word| word.eq_ignore_ascii_case("ONBUILD"))
    {
        problems.push(
            "check-pins doesn't read an ONBUILD's instruction, which another image's build runs: \
             write it in that build's Dockerfile (OPS-11)"
                .to_owned(),
        );
    }
    for package in unpinned_packages(words) {
        problems.push(format!(
            "the package {package} is pinned by version, as {package}=<version> (OPS-11)"
        ));
    }
    if picks_toolchain(line, words) {
        problems.push(
            "the image builds with the toolchain rust-toolchain.toml pins (CODE-01)".to_owned(),
        );
    }
    problems
}

/// Whether the command text `line`, split into `words`, names a toolchain in a form that
/// `check-pins` refuses: it holds `RUSTUP_TOOLCHAIN`, in any case, or [`names_cargo_toolchain`]
/// reads it, or it holds three words that [`toolchain_named`] reads, or a word whose
/// [`program`] is `rustup` whose next words [`rustup_picks`] reads.
pub(super) fn picks_toolchain(line: &str, words: &[&str]) -> bool {
    line.to_ascii_lowercase().contains("rustup_toolchain")
        || names_cargo_toolchain(line, words)
        || words.windows(3).any(toolchain_named)
        || (0..words.len())
            .any(|at| program(words[at]) == "rustup" && rustup_picks(&words[at + 1..]))
}

/// Whether the command text `line`, split into `words`, holds `cargo +`, or a word whose
/// [`program`] is `cargo` followed by one whose [`bare`] form starts with `+`.
pub(super) fn names_cargo_toolchain(line: &str, words: &[&str]) -> bool {
    let cargo_plus = |pair: &[&str]| program(pair[0]) == "cargo" && bare(pair[1]).starts_with('+');
    line.contains("cargo +") || words.windows(2).any(cargo_plus)
}

/// `word` as the shell passes it, as far as the check reads it: without its quotes and
/// backslashes.
pub(super) fn bare(word: &str) -> String {
    word.chars().filter(|c| !"\"'\\".contains(*c)).collect()
}

/// The program that `word` names, as a command's: the last part of its [`bare`] form after a
/// `/`, so `/usr/bin/apt-get`, `"apt-get"` and `\apt-get` each name `apt-get`.
fn program(word: &str) -> String {
    bare(word).rsplit('/').next().unwrap_or_default().to_owned()
}

/// The words of the command that `words` start, up to the first of [`ENDS`].
fn command<'w, 'a>(words: &'w [&'a str]) -> &'w [&'a str] {
    let end = words
        .iter()
        .position(|word| ENDS.contains(word))
        .unwrap_or(words.len());
    &words[..end]
}

/// Each [`command`] that the words after a word among `words` whose [`program`] is one of
/// `wanted` start, with the position in it of its first word whose [`bare`] form is `install`.
fn installs<'w, 'a>(words: &'w [&'a str], wanted: &[&str]) -> Vec<(&'w [&'a str], usize)> {
    (0..words.len())
        .filter(|at| wanted.contains(&program(words[*at]).as_str()))
        .filter_map(|at| {
            let command = command(&words[at + 1..]);
            let install = command.iter().position(|word| bare(word) == "install")?;
            Some((command, install))
        })
        .collect()
}

/// The packages that `apt-get` or `apt` [`installs`] without a version: each word after its
/// `install` that neither starts with `-` nor holds a `=`.
fn unpinned_packages<'a>(words: &[&'a str]) -> Vec<&'a str> {
    installs(words, &["apt-get", "apt"])
        .into_iter()
        .flat_map(|(command, install)| &command[install + 1..])
        .filter(|word| !word.starts_with('-') && !word.contains('='))
        .copied()
        .collect()
}

/// What is wrong with each `cargo install`, a command that `cargo` [`installs`]: a word after its
/// `install` that doesn't start with `-` and names no exact version, as `name@1.2.3`, and a
/// command without the word `--locked`.
fn cargo_install_problems(words: &[&str]) -> Vec<String> {
    installs(words, &["cargo"])
        .into_iter()
        .filter_map(|(command, install)| {
            let mut problems: Vec<String> = command[install + 1..]
                .iter()
                .filter(|arg| !arg.starts_with('-'))
                .filter(|arg| {
                    !arg.split_once('@')
                        .is_some_and(|(_, version)| is_exact_version(version))
                })
                .map(|arg| format!("{arg} names an exact version, as name@1.2.3"))
                .collect();
            if !command.contains(&"--locked") {
                problems.push("cargo install takes --locked".to_owned());
            }
            (!problems.is_empty()).then(|| format!("{} (OPS-11)", problems.join("; ")))
        })
        .collect()
}

/// Whether the words `after` a word whose [`program`] is `rustup` name its command `run` or
/// `override`, or `default` and a word other than those of [`ENDS`]: its command is the [`bare`]
/// form of its first word that starts with neither `-` nor `+`.
fn rustup_picks(after: &[&str]) -> bool {
    let mut rest = after.iter().skip_while(|word| word.starts_with(['-', '+']));
    match rest.next().map(|word| bare(word)).as_deref() {
        Some("run" | "override") => true,
        Some("default") => rest.next().is_some_and(|word| !ENDS.contains(word)),
        _ => false,
    }
}

/// Whether three words are `toolchain install` and a word other than those of [`ENDS`], as
/// `rustup toolchain install <toolchain>` ends, which would pick a toolchain other than
/// rust-toolchain.toml's.
fn toolchain_named(words: &[&str]) -> bool {
    matches!(words, ["toolchain", "install", toolchain] if !ENDS.contains(toolchain))
}
