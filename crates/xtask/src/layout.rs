//! `check-layout`: the code's files keep their places and their sizes.
//! - A Rust file of a crate's `src/`, before its test module, and a shell script under `deploy/`
//!   hold at most 500 lines (CODE-07). Test files, under a crate's `tests/`, aren't counted.
//! - A module's files come from ARCH-03's set, each a file or a directory of the same name, with
//!   the three entries that architecture.md §5 gives `access`, `auth` and `system`, and its email
//!   templates are in the directory of `crates/app/templates/` named after it (ARCH-03).
//! - Every module has its `error.rs` (ERR-01).
//! - No code of `platform` or `app` allows or expects the `dead_code` lint (CODE-14).
//! - A job is declared in its module's `jobs.rs`, or in an area of `platform`, and its kind starts
//!   with its module's name, or `platform.` for one of `platform`'s (OPS-09).

use crate::{Finding, Outcome, repo::Repo, shell};

/// The most lines a file holds (CODE-07).
const MOST_LINES: usize = 500;

/// The directory of `app`'s modules.
const MODULES: &str = "crates/app/src/modules/";

/// The directory of the modules' email templates.
const TEMPLATES: &str = "crates/app/templates/";

/// The code of `platform`.
const PLATFORM: &str = "crates/platform/src/";

/// The code of `app`.
const APP: &str = "crates/app/src/";

/// The files a module may hold, each a file or a directory of the same name (ARCH-03).
const MODULE_FILES: [&str; 12] = [
    "mod",
    "routes",
    "service",
    "repo",
    "model",
    "error",
    "events",
    "audit",
    "jobs",
    "emails",
    "metrics",
    "personal_data",
];

/// The entries that one module has besides the set (architecture.md §5).
const MODULE_EXTRAS: [(&str, &str); 3] = [
    ("access", "permissions"),
    ("auth", "providers"),
    ("system", "seed"),
];

/// Checks the repository's code against its layout.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let scripts = shell::scripts(repo);
    for file in repo.files() {
        let path = file.path.as_str();
        let in_src = path.starts_with("crates/")
            && path.split('/').nth(2) == Some("src")
            && path.ends_with(".rs");
        if in_src {
            let lines = lines_before_tests(&file.text);
            if lines > MOST_LINES {
                findings.push(Finding::new(
                    path,
                    MOST_LINES + 1,
                    format!(
                        "{lines} lines before its tests: split it into a directory of modules \
                         with the same name (CODE-07, ARCH-03)"
                    ),
                ));
            }
            if path.starts_with(PLATFORM) || path.starts_with(APP) {
                let code: Vec<&str> = file.text.lines().take(lines).collect();
                let code = code.join("\n");
                findings.extend(dead_code(path, &code));
                findings.extend(jobs(path, &code));
            }
        }
        if scripts.contains(&file.path) {
            let lines = file.text.lines().count();
            if lines > MOST_LINES {
                findings.push(Finding::new(
                    path,
                    MOST_LINES + 1,
                    format!(
                        "{lines} lines: split the script into files it sources, by task \
                         (CODE-07)"
                    ),
                ));
            }
        }
    }
    findings.extend(modules(repo));
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// The lines of `text` before its test module: a `#[cfg(test)]` line that a `mod` follows.
fn lines_before_tests(text: &str) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .windows(2)
        .position(|pair| {
            pair[0].trim() == "#[cfg(test)]" && pair[1].trim_start().starts_with("mod ")
        })
        .unwrap_or(lines.len())
}

/// ARCH-03 and ERR-01: each module's entries, its templates' directory, and its `error.rs`.
fn modules(repo: &Repo) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    for path in repo.paths() {
        if let Some(rest) = path.strip_prefix(MODULES) {
            let Some((module, inside)) = rest.split_once('/') else {
                continue;
            };
            if !names.contains(&module) {
                names.push(module);
            }
            let entry = inside.split('/').next().unwrap_or(inside);
            let stem = entry.strip_suffix(".rs").unwrap_or(entry);
            let allowed = MODULE_FILES.contains(&stem)
                || MODULE_EXTRAS
                    .iter()
                    .any(|(owner, extra)| *owner == module && *extra == stem);
            if !allowed {
                findings.push(Finding::new(
                    path,
                    1,
                    format!(
                        "`{entry}` isn't one of a module's files: {} (ARCH-03)",
                        MODULE_FILES.join(", ")
                    ),
                ));
            }
        }
    }
    for path in repo.paths() {
        if let Some(rest) = path.strip_prefix(TEMPLATES) {
            let directory = rest
                .split_once('/')
                .map_or(rest, |(directory, _)| directory);
            if !names.contains(&directory) {
                findings.push(Finding::new(
                    path,
                    1,
                    format!(
                        "the templates of `{directory}`, which is no module: a module's \
                         templates are in the directory named after it (ARCH-03)"
                    ),
                ));
            }
        }
    }
    for module in names {
        let error = format!("{MODULES}{module}/error.rs");
        if !repo.has(&error) && !repo.has(&format!("{MODULES}{module}/error")) {
            findings.push(Finding::new(
                &format!("{MODULES}{module}/mod.rs"),
                1,
                format!("`{module}` has no error.rs, where its error enum is (ERR-01)"),
            ));
        }
    }
    findings
}

/// CODE-14: each line of `text` that allows or expects `dead_code`, outside comments.
fn dead_code(path: &str, text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut attribute = false;
    for (index, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        if code.contains("#[allow(") || code.contains("#[expect(") {
            attribute = true;
        }
        if attribute && code.contains("dead_code") {
            findings.push(Finding::new(
                path,
                index + 1,
                "an exception to `dead_code`: delete the unused item instead (CODE-14)",
            ));
        }
        if code.contains(")]") {
            attribute = false;
        }
    }
    findings
}

/// OPS-09: a job of `app` is declared in its module's `jobs.rs`, and each kind starts with its
/// module's name, or `platform.` in `platform`.
fn jobs(path: &str, text: &str) -> Vec<Finding> {
    let module = path
        .strip_prefix(MODULES)
        .and_then(|rest| rest.split_once('/'))
        .map(|(module, rest)| (module, rest == "jobs.rs" || rest.starts_with("jobs/")));
    let prefix = match module {
        Some((module, _)) => format!("{module}."),
        None if path.starts_with(PLATFORM) => "platform.".to_owned(),
        None => String::new(),
    };
    let mut findings = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        let declares = code.contains("impl Job for")
            || code.contains(".queued(")
            || code.contains(".periodic(");
        let in_jobs = matches!(module, Some((_, true))) || path.starts_with(PLATFORM);
        if declares && !in_jobs {
            findings.push(Finding::new(
                path,
                index + 1,
                "a job is declared in its module's jobs.rs (OPS-09)",
            ));
        }
        let kind = code
            .split_once("const KIND: &'static str = \"")
            .or_else(|| code.split_once(".periodic(\""))
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(kind, _)| kind);
        if let Some(kind) = kind
            && !kind.starts_with(&prefix)
        {
            findings.push(Finding::new(
                path,
                index + 1,
                format!("the kind `{kind}` doesn't start with `{prefix}` (OPS-09)"),
            ));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(files: &[(&str, &str)]) -> Vec<String> {
        check(&Repo::of(files))
            .findings
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
            .collect()
    }

    #[test]
    fn code_07_a_file_over_500_lines_before_its_tests_fails() {
        let long = "fn a() {}\n".repeat(501);
        let tested = format!(
            "{}#[cfg(test)]\nmod tests {{\n{}}}\n",
            "fn a() {}\n".repeat(500),
            "fn t() {}\n".repeat(600)
        );
        let script = format!("#!/bin/sh\n{}", "true\n".repeat(500));
        let found = messages(&[
            ("crates/platform/src/long.rs", &long),
            ("crates/platform/src/tested.rs", &tested),
            ("crates/app/tests/integration/long.rs", &long),
            ("deploy/deploy.sh", &script),
        ]);
        assert_eq!(
            found,
            [
                "crates/platform/src/long.rs:501: 501 lines before its tests: split it into a \
                 directory of modules with the same name (CODE-07, ARCH-03)",
                "deploy/deploy.sh:501: 501 lines: split the script into files it sources, by \
                 task (CODE-07)",
            ]
        );
    }

    #[test]
    fn arch_03_a_module_holds_only_its_files_and_err_01_its_error() {
        let found = messages(&[
            ("crates/app/src/modules/things/mod.rs", ""),
            ("crates/app/src/modules/things/service/mod.rs", ""),
            ("crates/app/src/modules/things/helpers.rs", ""),
            ("crates/app/src/modules/auth/providers/github.rs", ""),
            ("crates/app/src/modules/auth/error.rs", ""),
            ("crates/app/src/modules/things/permissions.rs", ""),
            ("crates/app/templates/things/email.html", ""),
            ("crates/app/templates/gone/email.html", ""),
        ]);
        assert_eq!(found.len(), 4, "{found:#?}");
        for expected in [
            "crates/app/src/modules/things/helpers.rs:1: `helpers.rs` isn't one of a module's",
            "crates/app/src/modules/things/permissions.rs:1: `permissions.rs` isn't one of a",
            "crates/app/templates/gone/email.html:1: the templates of `gone`, which is no module",
            "crates/app/src/modules/things/mod.rs:1: `things` has no error.rs",
        ] {
            assert!(
                found.iter().any(|m| m.starts_with(expected)),
                "{expected} in {found:#?}"
            );
        }
    }

    #[test]
    fn code_14_no_exception_to_dead_code() {
        let found = messages(&[(
            "crates/platform/src/a.rs",
            "#[expect(\n    dead_code,\n    reason = \"later\"\n)]\nfn a() {}\n// dead_code in a comment\n",
        )]);
        assert_eq!(
            found,
            [
                "crates/platform/src/a.rs:2: an exception to `dead_code`: delete the unused item \
              instead (CODE-14)"
            ]
        );
    }

    #[test]
    fn ops_09_a_job_is_declared_in_jobs_rs_with_its_module_s_kind() {
        let found = messages(&[
            ("crates/app/src/modules/things/error.rs", ""),
            (
                "crates/app/src/modules/things/jobs.rs",
                "jobs.periodic(\"things.expire\", I, T, f);\njobs.periodic(\"other.expire\", I, T, f);\n",
            ),
            (
                "crates/app/src/modules/things/service.rs",
                "impl Job for Thing {\n    const KIND: &'static str = \"things.send\";\n}\n",
            ),
            (
                "crates/platform/src/mail/mod.rs",
                "impl Job for Delivery {\n    const KIND: &'static str = \"mail.send\";\n}\n",
            ),
        ]);
        assert_eq!(
            found,
            [
                "crates/app/src/modules/things/jobs.rs:2: the kind `other.expire` doesn't start \
                 with `things.` (OPS-09)",
                "crates/app/src/modules/things/service.rs:1: a job is declared in its module's \
                 jobs.rs (OPS-09)",
                "crates/platform/src/mail/mod.rs:2: the kind `mail.send` doesn't start with \
                 `platform.` (OPS-09)",
            ]
        );
    }
}
