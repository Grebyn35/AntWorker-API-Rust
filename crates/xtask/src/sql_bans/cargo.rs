//! The locked packages, as `cargo metadata` resolves them for the workspace with every feature,
//! as clippy builds it: where each package's sources are, the features it is built with, and the
//! packages its crate names.
//!
//! `cargo metadata` gives a package the features of every build of it, so a package that a
//! procedural macro also builds on, such as `sqlx-core`, has the macro's features too. The
//! features are a superset of those the API's build enables, so the check misses nothing it
//! compiles.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

/// A locked package.
pub struct Package {
    /// The package's name, such as `sqlx-core`.
    pub name: String,
    /// The directory that holds its manifest.
    pub directory: PathBuf,
    /// The root file of its library: `src/lib.rs`, unless its manifest names another.
    pub root: PathBuf,
    /// Whether its library is a procedural macro crate.
    pub proc_macro: bool,
    /// The features it is built with.
    pub features: BTreeSet<String>,
    /// The packages it depends on, by the name its code gives each crate, such as `sqlx_core`.
    pub dependencies: BTreeMap<String, String>,
}

/// The packages of the lock, by ID, and the ID of the `sqlx` that the workspace depends on.
pub struct Packages {
    pub packages: BTreeMap<String, Package>,
    pub sqlx: String,
}

/// Reads the lock through `cargo metadata`.
pub fn locked() -> Result<Packages, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--all-features",
        ])
        .current_dir(root)
        .output()
        .map_err(|error| format!("can't run cargo metadata: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("can't read cargo metadata's output: {error}"))?;
    parse(&metadata)
}

/// The packages that `metadata`, the output of `cargo metadata`, describes.
fn parse(metadata: &Value) -> Result<Packages, String> {
    let mut packages = BTreeMap::new();
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    for node in nodes {
        let id = text(&node["id"]);
        let Some(package) = metadata["packages"]
            .as_array()
            .and_then(|all| all.iter().find(|package| text(&package["id"]) == id))
        else {
            continue;
        };
        let manifest = PathBuf::from(text(&package["manifest_path"]));
        let library = package["targets"].as_array().and_then(|targets| {
            targets.iter().find(|target| {
                kinds(target)
                    .iter()
                    .any(|kind| kind == "lib" || kind == "proc-macro")
            })
        });
        let Some(library) = library else {
            continue;
        };
        packages.insert(
            id.clone(),
            Package {
                name: text(&package["name"]),
                directory: manifest.parent().map(Path::to_path_buf).unwrap_or_default(),
                root: PathBuf::from(text(&library["src_path"])),
                proc_macro: kinds(library).iter().any(|kind| kind == "proc-macro"),
                features: strings(&node["features"]).into_iter().collect(),
                dependencies: node["deps"]
                    .as_array()
                    .map_or(&[][..], Vec::as_slice)
                    .iter()
                    .map(|dependency| (text(&dependency["name"]), text(&dependency["pkg"])))
                    .collect(),
            },
        );
    }
    let members: BTreeSet<String> = strings(&metadata["workspace_members"])
        .into_iter()
        .collect();
    let sqlx = members
        .iter()
        .filter_map(|member| packages.get(member))
        .find_map(|member| member.dependencies.get("sqlx"))
        .cloned()
        .ok_or("no crate of the workspace depends on sqlx")?;
    Ok(Packages { packages, sqlx })
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .map(text)
        .collect()
}

fn kinds(target: &Value) -> Vec<String> {
    strings(&target["kind"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_20_the_sqlx_the_workspace_depends_on_is_found_with_its_features() {
        let metadata = serde_json::json!({
            "packages": [
                {"id": "app", "name": "app", "manifest_path": "/w/app/Cargo.toml",
                 "targets": [{"kind": ["lib"], "src_path": "/w/app/src/lib.rs"}]},
                {"id": "sqlx 1", "name": "sqlx", "manifest_path": "/r/sqlx-1/Cargo.toml",
                 "targets": [{"kind": ["lib"], "src_path": "/r/sqlx-1/src/lib.rs"},
                             {"kind": ["test"], "src_path": "/r/sqlx-1/tests/a.rs"}]},
                {"id": "macros 1", "name": "sqlx-macros", "manifest_path": "/r/m/Cargo.toml",
                 "targets": [{"kind": ["proc-macro"], "src_path": "/r/m/src/lib.rs"}]}
            ],
            "workspace_members": ["app"],
            "resolve": {"nodes": [
                {"id": "app", "features": [], "deps": [{"name": "sqlx", "pkg": "sqlx 1"}]},
                {"id": "sqlx 1", "features": ["postgres", "macros"],
                 "deps": [{"name": "sqlx_macros", "pkg": "macros 1"}]},
                {"id": "macros 1", "features": [], "deps": []}
            ]}
        });

        let packages = parse(&metadata).unwrap();

        let sqlx = &packages.packages[&packages.sqlx];
        assert_eq!(sqlx.name, "sqlx");
        assert_eq!(sqlx.root, PathBuf::from("/r/sqlx-1/src/lib.rs"));
        assert_eq!(sqlx.directory, PathBuf::from("/r/sqlx-1"));
        assert!(sqlx.features.contains("postgres") && !sqlx.proc_macro);
        assert_eq!(sqlx.dependencies["sqlx_macros"], "macros 1");
        assert!(packages.packages["macros 1"].proc_macro);
    }
}
