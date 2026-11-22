//! `inputs`: an identifier of the files under a directory, as they stand in the working tree.
//! The same files, with the same contents, give the same identifier on any machine, so the image
//! built from them carries it, as its label `build.inputs`, and a deploy can tell an image built
//! from other files (OPS-14). git computes it: the hash of each file's contents, then the hash
//! of the list of paths with their hashes.
//!
//! `image-inputs`, which `just e2e` and `just load-test` run before they run the API's image in
//! `target/image`: it fails unless that image carries the identifier of the files that the
//! working tree would build it from, so no run measures an image of earlier code (OPS-20).

use std::{
    io::Write,
    path::Path,
    process::{Command, ExitCode, Stdio},
};

use serde_json::Value;

/// The files that the API's image is built from, as git's pathspecs: the `Dockerfile`, and what
/// its `.dockerignore` lets into the build's context, without what no build of the API's binary
/// reads, the tests of `app` and all of `xtask` but its manifest, which the workspace names.
pub const API_IMAGE: &[&str] = &[
    "Dockerfile",
    ".dockerignore",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".env.example",
    ".sqlx",
    "migrations",
    "crates/app",
    "crates/platform",
    "crates/xtask/Cargo.toml",
    ":!crates/app/tests",
];

/// The label of an image that holds the identifier of the files it is built from.
const LABEL: &str = "build.inputs";

/// The image layout that `just image` writes the API's image to.
const IMAGE: &str = "target/image";

/// Prints the identifier of the files under `directory`, a path from the repository root, or,
/// without one, of the files that the API's image is built from.
pub fn run(directory: Option<&str>) -> ExitCode {
    let (paths, named) = match directory {
        Some(directory) => (vec![directory], directory),
        None => (API_IMAGE.to_vec(), "the API's image"),
    };
    match identify(&paths) {
        Ok(identifier) => {
            #[expect(clippy::print_stdout, reason = "the identifier is the tool's output")]
            {
                println!("{identifier}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            #[expect(clippy::print_stderr, reason = "the error is the tool's output")]
            {
                eprintln!("can't identify the files of {named}: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

/// Fails unless the API's image in `target/image` carries, as its label `build.inputs`, the
/// identifier of the files that the working tree would build it from.
pub fn check_image() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let layout = root.join(IMAGE);
    let read = |name: &str| -> Option<String> {
        let bytes = crate::repo::read(&layout.join(name)).ok()?;
        String::from_utf8(bytes).ok()
    };
    let problem = match (identify(API_IMAGE), label(&read)) {
        (Err(error), _) => format!("can't identify the files of the API's image: {error}"),
        (Ok(_), None) => format!(
            "{IMAGE} holds no image that says which files it is built from, as its label \
             `{LABEL}`: run `just image`"
        ),
        (Ok(tree), Some(built)) if tree != built => format!(
            "the image in {IMAGE} is built from other files than the working tree holds: run \
             `just image`"
        ),
        (Ok(_), Some(_)) => return ExitCode::SUCCESS,
    };
    #[expect(clippy::print_stderr, reason = "the error is the tool's output")]
    {
        eprintln!("{problem}");
    }
    ExitCode::FAILURE
}

/// The label `build.inputs` of the image of an OCI image layout, whose file at a path from the
/// layout's root `read` gives: of the first manifest that the layout's index names, and, where
/// that is an index of several platforms' images, of the first of them that names a platform's
/// architecture, since each platform's image carries the label. `None` where the layout holds
/// no such image, or its image no such label.
fn label(read: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let json = |name: &str| serde_json::from_str::<Value>(&read(name)?).ok();
    let blob = |descriptor: &Value| {
        let digest = descriptor["digest"].as_str()?;
        json(&format!("blobs/{}", digest.replacen(':', "/", 1)))
    };
    let mut manifest = blob(json("index.json")?["manifests"].get(0)?)?;
    if let Some(images) = manifest["manifests"].as_array() {
        let image = images.iter().find(|image| {
            image["platform"]["architecture"]
                .as_str()
                .is_some_and(|architecture| architecture != "unknown")
        })?;
        manifest = blob(image)?;
    }
    let config = blob(&manifest["config"])?;
    config["config"]["Labels"][LABEL]
        .as_str()
        .map(str::to_owned)
}

/// The identifier of the files that `paths`, git's pathspecs from the repository root, name.
fn identify(paths: &[&str]) -> Result<String, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let listing = [
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
        ],
        paths,
    ];
    let listed = git(&root, &listing.concat(), "")?;
    // git still lists a tracked file deleted from the working tree until the deletion is staged.
    let deleted = git(
        &root,
        &[&["ls-files", "--deleted", "--"], paths].concat(),
        "",
    )?;
    let deleted: Vec<&str> = deleted.lines().collect();
    let mut paths: Vec<&str> = listed
        .lines()
        .filter(|path| !deleted.contains(path))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    if paths.is_empty() {
        return Err("no file".to_owned());
    }
    let hashes = git(
        &root,
        &["hash-object", "--stdin-paths"],
        &(paths.join("\n") + "\n"),
    )?;
    let listing: String = paths
        .iter()
        .zip(hashes.lines())
        .map(|(path, hash)| format!("{hash} {path}\n"))
        .collect();
    Ok(git(&root, &["hash-object", "--stdin"], &listing)?
        .trim()
        .to_owned())
}

/// Runs git with `args` in `root`, writing `input` to its standard input, and returns what it
/// wrote to its standard output.
fn git(root: &Path, args: &[&str], input: &str) -> Result<String, String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't run git: {error}"))?;
    child
        .stdin
        .take()
        .ok_or("git has no standard input")?
        .write_all(input.as_bytes())
        .map_err(|error| format!("can't write to git: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("git failed: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    String::from_utf8(output.stdout).map_err(|_| "git's output isn't UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn ops_14_the_same_files_give_the_same_identifier_and_others_another() {
        let postgres = identify(&["deploy/postgres"]).unwrap();

        assert_eq!(identify(&["deploy/postgres"]).unwrap(), postgres);
        assert_ne!(identify(&["deploy/observability"]).unwrap(), postgres);
        assert_eq!(postgres.len(), 40);
        assert!(identify(&["no/such/directory"]).is_err());
    }

    #[test]
    fn ops_20_the_api_image_s_inputs_are_its_build_s_context_without_the_tests() {
        let with_tests: Vec<&str> = API_IMAGE
            .iter()
            .copied()
            .filter(|path| !path.starts_with(":!"))
            .collect();

        assert_ne!(identify(API_IMAGE).unwrap(), identify(&with_tests).unwrap());
        // Each entry that the `.dockerignore` lets into the context is among the inputs, itself
        // or as the directories below it.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let ignore = String::from_utf8(crate::repo::read(&root.join(".dockerignore")).unwrap());
        for entry in ignore
            .unwrap()
            .lines()
            .filter_map(|line| line.strip_prefix('!'))
        {
            let below = format!("{entry}/");
            assert!(
                API_IMAGE
                    .iter()
                    .any(|path| *path == entry || path.starts_with(&below)),
                "{entry}"
            );
        }
    }

    #[test]
    fn ops_20_an_image_s_inputs_are_the_label_of_its_first_platform_s_image() {
        let layout = |labels: Value| {
            move |name: &str| -> Option<String> {
                let blob = match name {
                    "index.json" => json!({ "manifests": [{ "digest": "sha256:aa" }] }),
                    "blobs/sha256/aa" => json!({ "manifests": [
                        { "digest": "sha256:a0", "platform": { "architecture": "unknown" } },
                        { "digest": "sha256:a1", "platform": { "architecture": "amd64" } },
                        { "digest": "sha256:a2", "platform": { "architecture": "arm64" } },
                    ] }),
                    "blobs/sha256/a1" => json!({ "config": { "digest": "sha256:c1" } }),
                    "blobs/sha256/c1" => json!({ "config": { "Labels": labels } }),
                    _ => return None,
                };
                Some(blob.to_string())
            }
        };

        assert_eq!(
            label(&layout(json!({ "build.inputs": "0a1b" }))).as_deref(),
            Some("0a1b")
        );
        assert_eq!(label(&layout(json!({ "other": "0a1b" }))), None);
        assert_eq!(label(&layout(json!(null))), None);
        assert_eq!(label(&|_| None), None);
        // A layout of one platform's image, whose index names the image itself.
        let single = |name: &str| -> Option<String> {
            let blob = match name {
                "index.json" => json!({ "manifests": [{ "digest": "sha256:a1" }] }),
                "blobs/sha256/a1" => json!({ "config": { "digest": "sha256:c1" } }),
                "blobs/sha256/c1" => json!({ "config": { "Labels": { "build.inputs": "9f" } } }),
                _ => return None,
            };
            Some(blob.to_string())
        };
        assert_eq!(label(&single).as_deref(), Some("9f"));
    }
}
