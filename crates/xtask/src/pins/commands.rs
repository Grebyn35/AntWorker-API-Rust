//! What `check-pins` reads of the commands of a Dockerfile's instruction: the crates and the
//! packages that it installs, the toolchain that it picks, and the forms that the check doesn't
//! read.

use super::{forms::is_exact_version, instructions::is_exec_form};

/// What is wrong with the commands of the Dockerfile instruction `line`, split into `words`: a
/// crate or a package installed without its version, a toolchain other than the pinned one, a
/// `RUN` or an `ADD` in the exec form, as [`is_exec_form`] reads it, or an instruction whose
/// first word is `ONBUILD`, in any case, which the check doesn't read.
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
    let cargo_plus = |pair: &[&str]| program(pair[0]) == "cargo" && bare(pair[1]).starts_with('+');
    if line.contains("cargo +")
        || words.windows(2).any(cargo_plus)
        || words.windows(3).any(toolchain_named)
    {
        problems.push(
            "the image builds with the toolchain rust-toolchain.toml pins (CODE-01)".to_owned(),
        );
    }
    problems
}

/// `word` as the shell passes it, as far as the check reads it: without its quotes and
/// backslashes.
fn bare(word: &str) -> String {
    word.chars().filter(|c| !"\"'\\".contains(*c)).collect()
}

/// The program that `word` names, as a command's: the last part of its [`bare`] form after a
/// `/`, so `/usr/bin/apt-get`, `"apt-get"` and `\apt-get` each name `apt-get`.
fn program(word: &str) -> String {
    bare(word).rsplit('/').next().unwrap_or_default().to_owned()
}

/// The packages that each word among `words` whose [`program`] is `apt-get` or `apt` installs
/// without a version: each word after the first whose [`bare`] form is `install`, among the
/// words of its command, which ends at `&&`, `;`, `\` or `||`, that neither starts with `-` nor
/// holds a `=`.
fn unpinned_packages<'a>(words: &[&'a str]) -> Vec<&'a str> {
    let mut packages = Vec::new();
    for (at, word) in words.iter().enumerate() {
        if !matches!(program(word).as_str(), "apt-get" | "apt") {
            continue;
        }
        let mut command = words[at + 1..]
            .iter()
            .take_while(|word| !["&&", ";", "\\", "||"].contains(word))
            .skip_while(|word| bare(word) != "install");
        if command.next().is_some() {
            packages.extend(
                command
                    .filter(|word| !word.starts_with('-') && !word.contains('='))
                    .copied(),
            );
        }
    }
    packages
}

/// What is wrong with each `cargo install` among `words`: two words, the first of which has the
/// [`program`] `cargo` and the second the [`bare`] form `install`.
fn cargo_install_problems(words: &[&str]) -> Vec<String> {
    words
        .windows(2)
        .enumerate()
        .filter(|(_, pair)| program(pair[0]) == "cargo" && bare(pair[1]) == "install")
        .filter_map(|(at, _)| cargo_install_problem(&words[at + 2..]))
        .collect()
}

/// What is wrong with the `cargo install` whose arguments `args` start, if anything.
fn cargo_install_problem(args: &[&str]) -> Option<String> {
    let args = args
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
    (!problems.is_empty()).then(|| format!("{} (OPS-11)", problems.join("; ")))
}

/// Whether three words are one whose [`program`] is `rustup`, `default` and another, or
/// `toolchain install` and a word other than `&&`, `;` and `\`, as `rustup toolchain install
/// <toolchain>` ends, which would pick a toolchain other than rust-toolchain.toml's.
fn toolchain_named(words: &[&str]) -> bool {
    match words {
        [rustup, "default", _] => program(rustup) == "rustup",
        ["toolchain", "install", toolchain] => !matches!(*toolchain, "&&" | ";" | "\\"),
        _ => false,
    }
}
