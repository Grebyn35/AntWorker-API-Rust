//! `check-deps`, for the crates' dependencies: each dependency a crate of the workspace declares
//! is named in the code that its kind builds (CODE-11), so a dependency that no code uses any
//! more, such as one that only a removed optional part used, fails rather than staying in the
//! build. A normal dependency is named in the crate's `src/` or build script, a development
//! dependency there or in its tests, examples and benches, and a build dependency in its build
//! script. Cargo's metadata gives the dependencies, each under the name its code uses.

use std::process::Command;

use serde_json::Value;

use crate::{Finding, repo::Repo};

/// One dependency that a crate declares.
#[derive(Debug, PartialEq, Eq)]
struct Dependency {
    /// The crate's directory, from the repository root, such as `crates/app`.
    krate: String,
    /// The name its code uses: the dependency's rename, or its name, with `-` as `_`.
    name: String,
    /// `normal`, `dev` or `build`.
    kind: String,
}

/// A finding for each dependency of a crate of the workspace that the code its kind builds never
/// names.
pub fn check(repo: &Repo) -> Vec<Finding> {
    match dependencies() {
        Ok(dependencies) => unused(repo, &dependencies),
        Err(error) => vec![Finding::new("Cargo.toml", 1, error)],
    }
}

/// The dependencies of the workspace's crates, from Cargo's metadata.
fn dependencies() -> Result<Vec<Dependency>, String> {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--no-deps", "--locked"])
        .output()
        .map_err(|error| format!("can't run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("cargo metadata isn't JSON: {error}"))?;
    let root = metadata["workspace_root"].as_str().unwrap_or_default();
    let mut found = Vec::new();
    for package in metadata["packages"].as_array().into_iter().flatten() {
        let manifest = package["manifest_path"].as_str().unwrap_or_default();
        let krate = manifest
            .strip_prefix(root)
            .unwrap_or(manifest)
            .trim_start_matches('/')
            .trim_end_matches("Cargo.toml")
            .trim_end_matches('/')
            .to_owned();
        for dependency in package["dependencies"].as_array().into_iter().flatten() {
            let name = dependency["rename"]
                .as_str()
                .or_else(|| dependency["name"].as_str())
                .unwrap_or_default()
                .replace('-', "_");
            let kind = dependency["kind"].as_str().unwrap_or("normal").to_owned();
            found.push(Dependency {
                krate: krate.clone(),
                name,
                kind,
            });
        }
    }
    Ok(found)
}

/// Each of `dependencies` that no Rust file its kind builds names, as a word.
fn unused(repo: &Repo, dependencies: &[Dependency]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for dependency in dependencies {
        let places: &[&str] = match dependency.kind.as_str() {
            "dev" => &["src/", "build.rs", "tests/", "examples/", "benches/"],
            "build" => &["build.rs"],
            _ => &["src/", "build.rs"],
        };
        let named = repo
            .files()
            .filter(|file| file.path.ends_with(".rs"))
            .filter(|file| {
                file.path
                    .strip_prefix(&format!("{}/", dependency.krate))
                    .is_some_and(|rest| places.iter().any(|place| rest.starts_with(place)))
            })
            .any(|file| names(&file.text, &dependency.name));
        if !named {
            let manifest = format!("{}/Cargo.toml", dependency.krate);
            let line = repo
                .file(&manifest)
                .and_then(|file| {
                    let hyphenated = dependency.name.replace('_', "-");
                    file.text.lines().position(|line| {
                        let key = line.split(['=', '.']).next().unwrap_or_default().trim();
                        key == dependency.name || key == hyphenated
                    })
                })
                .map_or(1, |index| index + 1);
            findings.push(Finding::new(
                &manifest,
                line,
                format!(
                    "no code of the crate names `{}`, a {} dependency: remove it (CODE-11)",
                    dependency.name, dependency.kind
                ),
            ));
        }
    }
    findings
}

/// Whether `text` holds `name` as a word of its own.
fn names(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(start, _)| {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        !text[..start].ends_with(word) && !text[start + name.len()..].starts_with(word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(name: &str, kind: &str) -> Dependency {
        Dependency {
            krate: "crates/app".to_owned(),
            name: name.to_owned(),
            kind: kind.to_owned(),
        }
    }

    #[test]
    fn code_11_a_dependency_that_no_code_of_its_kind_names_fails() {
        let repo = Repo::of(&[
            (
                "crates/app/Cargo.toml",
                "[dependencies]\nserde_json.workspace = true\nold-crate = \"1\"\n\
                 [dev-dependencies]\njsonschema.workspace = true\nunused_dev = \"1\"\n",
            ),
            (
                "crates/app/src/lib.rs",
                "use serde_json::Value;\n// jsonschema\n",
            ),
            (
                "crates/app/tests/contract.rs",
                "fn f() { jsonschema::validate(); }\n",
            ),
            ("crates/platform/src/lib.rs", "use old_crate::X;\n"),
        ]);
        let dependencies = [
            dependency("serde_json", "normal"),
            dependency("old_crate", "normal"),
            dependency("jsonschema", "dev"),
            dependency("unused_dev", "dev"),
        ];

        let messages: Vec<String> = unused(&repo, &dependencies)
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
            .collect();

        assert_eq!(
            messages,
            [
                "crates/app/Cargo.toml:3: no code of the crate names `old_crate`, a normal \
                 dependency: remove it (CODE-11)",
                "crates/app/Cargo.toml:6: no code of the crate names `unused_dev`, a dev \
                 dependency: remove it (CODE-11)",
            ]
        );
    }
}
