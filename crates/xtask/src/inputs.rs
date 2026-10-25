//! `inputs`: an identifier of the files under a directory, as they stand in the working tree.
//! The same files, with the same contents, give the same identifier on any machine, so the image
//! built from them carries it, and a deploy can tell an image built from other files (OPS-16).
//! git computes it: the hash of each file's contents, then the hash of the list of paths with
//! their hashes.

use std::{
    io::Write,
    path::Path,
    process::{Command, ExitCode, Stdio},
};

/// Prints the identifier of the files under `directory`, a path from the repository root.
pub fn run(directory: &str) -> ExitCode {
    match identify(directory) {
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
                eprintln!("can't identify the files under {directory}: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn identify(directory: &str) -> Result<String, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let listed = git(
        &root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            directory,
        ],
        "",
    )?;
    // git still lists a tracked file deleted from the working tree until the deletion is staged.
    let deleted = git(&root, &["ls-files", "--deleted", "--", directory], "")?;
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
    use super::*;

    #[test]
    fn ops_16_the_same_files_give_the_same_identifier_and_others_another() {
        let postgres = identify("deploy/postgres").unwrap();

        assert_eq!(identify("deploy/postgres").unwrap(), postgres);
        assert_ne!(identify("deploy/observability").unwrap(), postgres);
        assert_eq!(postgres.len(), 40);
        assert!(identify("no/such/directory").is_err());
    }
}
