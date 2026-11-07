//! `check-layout`: the code's files keep their places and their sizes.
//! - A Rust file of a crate's `src/`, before its test module, a shell script under `deploy/`, and
//!   the justfile and each file it imports hold at most 500 lines (CODE-08). Test files, under a
//!   crate's `tests/`, aren't counted.
//! - A module's files come from ARCH-03's set, each a file or a directory of the same name, with
//!   the three entries that architecture.md §5 gives `access`, `auth` and `system`, and its email
//!   templates are in the directory of `crates/app/templates/` named after it (ARCH-03).
//! - Every module has its `error.rs` (ERR-01).
//! - No code of `platform` or `app` allows or expects the `dead_code` lint, and no public item
//!   of `platform` is one that only tests name (CODE-15, in `unused`).
//! - A job is declared in its module's `jobs.rs`, or in an area of `platform`, and its kind starts
//!   with its module's name, or `platform.` for one of `platform`'s (OPS-09).

use crate::{Finding, Outcome, repo::Repo, shell, text, unused};

/// The most lines a file holds (CODE-08).
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
const MODULE_EXTRAS: [(&str, &str); 4] = [
    ("access", "permissions"),
    ("auth", "providers"),
    ("files", "restore"),
    ("system", "seed"),
];

/// Checks the repository's code against its layout.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let scripts = shell::scripts(repo);
    let justfiles: Vec<&str> = shell::justfiles(repo)
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    for file in repo.files() {
        let path = file.path.as_str();
        let in_src = path.starts_with("crates/")
            && path.split('/').nth(2) == Some("src")
            && path.ends_with(".rs");
        if in_src {
            let lines = text::lines_before_tests(&file.text);
            if lines > MOST_LINES {
                findings.push(Finding::new(
                    path,
                    MOST_LINES + 1,
                    format!(
                        "{lines} lines before its tests: split it into a directory of modules \
                         with the same name (CODE-08, ARCH-03)"
                    ),
                ));
            }
            if path.starts_with(PLATFORM) || path.starts_with(APP) {
                let code: Vec<&str> = file.text.lines().take(lines).collect();
                findings.extend(jobs(path, &code.join("\n")));
                findings.extend(templates(path, &code));
            }
        }
        let tested = ["crates/platform/", "crates/app/"]
            .iter()
            .any(|root| path.starts_with(root));
        if tested && path.ends_with(".rs") {
            findings.extend(dead_code(path, &file.text));
        }
        if justfiles.contains(&path) {
            findings.extend(tasks(path, &file.text));
            let lines = file.text.lines().count();
            if lines > MOST_LINES {
                findings.push(Finding::new(
                    path,
                    MOST_LINES + 1,
                    format!(
                        "{lines} lines: split its tasks into more files of `just/`, which the justfile \
                         imports, by kind (CODE-08)"
                    ),
                ));
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
                         (CODE-08)"
                    ),
                ));
            }
        }
    }
    findings.extend(modules(repo));
    findings.extend(unused::check(repo));
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// CODE-08: the tasks of a file that the justfile imports, `just/<group>.just`, are those of its
/// group; and the justfile's own each run others, as dependencies or through `just`.
fn tasks(path: &str, text: &str) -> Vec<Finding> {
    let own = path
        .strip_prefix("just/")
        .and_then(|name| name.strip_suffix(".just"));
    let lines: Vec<&str> = text.lines().collect();
    let mut findings = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(group) = line
            .strip_prefix("[group('")
            .and_then(|rest| rest.strip_suffix("')]"))
            && own.is_some_and(|own| own != group)
        {
            findings.push(Finding::new(
                path,
                index + 1,
                format!("a task of the group `{group}` goes in just/{group}.just (CODE-08)"),
            ));
        }
        let header = line.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            && !line.contains(":=")
            && !line.starts_with("import ")
            && !line.starts_with("set ")
            && !line.starts_with("export ");
        let Some((_, dependencies)) = line.split_once(':').filter(|_| header && own.is_none())
        else {
            continue;
        };
        let runs = lines[index + 1..]
            .iter()
            .take_while(|body| body.starts_with(' '))
            .any(|body| {
                body.trim_start()
                    .trim_start_matches('@')
                    .starts_with("just ")
            });
        if dependencies.trim().is_empty() && !runs {
            findings.push(Finding::new(
                path,
                index + 1,
                "a task that runs no other goes in the file of `just/` named after its group \
                 (CODE-08)",
            ));
        }
    }
    findings
}

/// ARCH-03: a module's emails render the templates of its own directory of
/// `crates/app/templates/`.
fn templates(path: &str, code: &[&str]) -> Vec<Finding> {
    let Some(module) = path
        .strip_prefix(MODULES)
        .and_then(|rest| rest.split_once('/'))
        .map(|(module, _)| module)
    else {
        return Vec::new();
    };
    code.iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let template = line.split_once("template(path = \"")?.1;
            let directory = template
                .split_once('/')
                .map_or("", |(directory, _)| directory);
            (directory != module).then(|| {
                Finding::new(
                    path,
                    index + 1,
                    format!(
                        "`{module}` renders a template outside crates/app/templates/{module}/, \
                         the directory of its own templates (ARCH-03)"
                    ),
                )
            })
        })
        .collect()
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

/// CODE-15: each line of `text` that allows or expects `dead_code`, outside comments, in an outer
/// attribute or an inner one, but on a test target's module that another target's support holds,
/// which `#[path]` names: that target, which holds the module, checks its every item.
fn dead_code(path: &str, text: &str) -> Vec<Finding> {
    let lines: Vec<&str> = text
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .collect();
    let test_target = path.split('/').nth(2) == Some("tests");
    let mut findings = Vec::new();
    let mut attribute: Option<bool> = None;
    let mut found = Vec::new();
    for (index, code) in lines.iter().enumerate() {
        // An attribute that allows or expects lints, `cfg_attr`'s among them.
        let excepts = code.contains("allow(") || code.contains("expect(");
        if excepts && (code.contains("#[") || code.contains("#![")) {
            attribute = Some(code.contains("#["));
        }
        // `unused` is the lint group that holds `dead_code`.
        let names = code
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|word| word == "dead_code" || word == "unused");
        if attribute.is_some() && names {
            found.push(index);
        }
        if code.contains(")]")
            && let Some(outer) = attribute.take()
        {
            let shared = outer && test_target && shared_module(&lines[index + 1..]);
            if !shared {
                findings.extend(found.iter().map(|line| {
                    Finding::new(
                        path,
                        line + 1,
                        "an exception to `dead_code`: delete the unused item instead (CODE-15)",
                    )
                }));
            }
            found.clear();
        }
    }
    findings
}

/// Whether the item that `lines` start with, after its other attributes, is a module that
/// `#[path]` takes from elsewhere.
fn shared_module(lines: &[&str]) -> bool {
    let mut path = false;
    for line in lines.iter().map(|line| line.trim()) {
        if line.starts_with("#[path") {
            path = true;
        } else if !line.starts_with("#[") {
            return path && (line.starts_with("mod ") || line.starts_with("pub mod "));
        }
    }
    false
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
    // A file of `platform` that fills the registry itself, as its sweeps do, rather than through
    // the registry's own `periodic` and `queued`, names each kind in a literal apart from the
    // call that registers it, so each literal that could be a kind is read as one.
    let fills = path.starts_with(PLATFORM)
        && (text.contains("periodic.insert(") || text.contains("queued.insert("))
        && !text.contains("fn periodic")
        && !text.contains("fn queued");
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
        let declared = code
            .split_once("const KIND: &'static str = \"")
            .or_else(|| code.split_once(".periodic(\""))
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(kind, _)| kind);
        let literals = if fills {
            kind_literals(code)
        } else {
            Vec::new()
        };
        for kind in declared.into_iter().chain(literals) {
            if !kind.starts_with(&prefix) {
                findings.push(Finding::new(
                    path,
                    index + 1,
                    format!("the kind `{kind}` doesn't start with `{prefix}` (OPS-09)"),
                ));
            }
        }
    }
    findings
}

/// The string literals of `code` that could be kinds: `snake_case` words joined by dots.
fn kind_literals(code: &str) -> Vec<&str> {
    let word = |text: &str| {
        text.starts_with(|c: char| c.is_ascii_lowercase())
            && text
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    code.split('"')
        .skip(1)
        .step_by(2)
        .filter(|literal| literal.split('.').all(word))
        .collect()
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
    fn code_08_a_file_over_500_lines_before_its_tests_fails() {
        let long = "fn a() {}\n".repeat(501);
        let tested = format!(
            "{}#[cfg(test)]\npub mod tests {{\n{}}}\n",
            "fn a() {}\n".repeat(500),
            "fn t() {}\n".repeat(600)
        );
        let script = format!("#!/bin/sh\n{}", "true\n".repeat(500));
        let justfile = format!("import 'just/check.just'\n{}", "# a\n".repeat(499));
        let tasks = format!("[group('check')]\ncheck:\n{}", "    true\n".repeat(499));
        let found = messages(&[
            ("crates/platform/src/long.rs", &long),
            ("crates/platform/src/tested.rs", &tested),
            ("crates/app/tests/integration/long.rs", &long),
            ("deploy/deploy.sh", &script),
            ("justfile", &justfile),
            ("just/check.just", &tasks),
            ("just/unused.just", &tasks),
        ]);
        assert_eq!(
            found,
            [
                "crates/platform/src/long.rs:501: 501 lines before its tests: split it into a \
                 directory of modules with the same name (CODE-08, ARCH-03)",
                "deploy/deploy.sh:501: 501 lines: split the script into files it sources, by \
                 task (CODE-08)",
                "just/check.just:501: 501 lines: split its tasks into more files of `just/`, \
                 which the justfile imports, by kind (CODE-08)",
            ]
        );
    }

    #[test]
    fn code_08_each_file_of_just_holds_its_group_s_tasks() {
        let justfile = "set lazy\nimport 'just/check.just'\n\n[private]\ndefault:\n    @just --list\n\n\
                        [group('check')]\nci: checks image\n\nlonely:\n    cargo build\n";
        let check = "# The checks.\n\n[group('check')]\nlint:\n    cargo clippy\n\n\
                     [group('image')]\nimage:\n    docker build .\n\n[private]\n_helper:\n    true\n";
        let found = messages(&[("justfile", justfile), ("just/check.just", check)]);
        assert_eq!(
            found,
            [
                "justfile:11: a task that runs no other goes in the file of `just/` named after \
                 its group (CODE-08)",
                "just/check.just:7: a task of the group `image` goes in just/image.just (CODE-08)",
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
            (
                "crates/app/src/modules/things/emails.rs",
                "#[template(path = \"things/email.html\")]\n#[template(path = \"others/email.html\")]\n",
            ),
        ]);
        assert_eq!(found.len(), 5, "{found:#?}");
        for expected in [
            "crates/app/src/modules/things/emails.rs:2: `things` renders a template outside",
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
    fn code_15_no_exception_to_dead_code() {
        let shared = "#[cfg(test)]\n#[expect(\n    dead_code,\n    reason = \"the other target's\"\n)]\n\
                      #[path = \"../integration/support/mod.rs\"]\nmod support;\n";
        let found = messages(&[
            (
                "crates/platform/src/a.rs",
                "#[expect(\n    dead_code,\n    reason = \"later\"\n)]\nfn a() {}\n// dead_code in a comment\n",
            ),
            (
                "crates/platform/src/b.rs",
                "#![expect(dead_code, reason = \"later\")]\nfn b() {}\n\
                 #[expect(unused, reason = \"later\")]\nfn d() {}\n\
                 #[cfg_attr(test, allow(dead_code))]\nfn e() {}\n\
                 #[expect(unused_imports, reason = \"another lint\")]\nuse x::y;\n",
            ),
            (
                "crates/app/tests/integration/main.rs",
                "#[allow(dead_code)]\nmod support;\n",
            ),
            ("crates/app/tests/e2e/main.rs", shared),
            (
                "crates/app/src/c.rs",
                &shared.replace("mod support;", "fn c() {}"),
            ),
        ]);
        let lines: Vec<&str> = found
            .iter()
            .map(|message| message.split(": ").next().unwrap_or_default())
            .collect();
        assert_eq!(
            lines,
            [
                "crates/platform/src/a.rs:2",
                "crates/platform/src/b.rs:1",
                "crates/platform/src/b.rs:3",
                "crates/platform/src/b.rs:5",
                "crates/app/tests/integration/main.rs:1",
                "crates/app/src/c.rs:3",
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
            (
                "crates/platform/src/jobs/sweeps.rs",
                "for (name, sweep) in [\n    (\"platform.expire\", A),\n    (\"expire_b\", B),\n    \
                 (\"sweeps.c\", C),\n] {\n    jobs.periodic.insert(name, sweep);\n}\n",
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
                "crates/platform/src/jobs/sweeps.rs:3: the kind `expire_b` doesn't start with \
                 `platform.` (OPS-09)",
                "crates/platform/src/jobs/sweeps.rs:4: the kind `sweeps.c` doesn't start with \
                 `platform.` (OPS-09)",
            ]
        );
    }
}
