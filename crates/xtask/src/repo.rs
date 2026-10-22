//! The files of the repository.

use std::{io, path::Path, process::Command};

/// A text file of the repository.
pub struct File {
    /// The path from the repository root, with `/` between its parts.
    pub path: String,
    /// The contents.
    pub text: String,
}

/// The repository's text files: every UTF-8 file that git tracks, or would track once added.
pub struct Repo {
    files: Vec<File>,
}

impl Repo {
    /// The files of the repository that holds this crate.
    pub fn load() -> io::Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let listing = Command::new("git")
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .current_dir(&root)
            .output()?;
        if !listing.status.success() {
            return Err(io::Error::other("git ls-files failed"));
        }
        let mut files = Vec::new();
        for path in String::from_utf8_lossy(&listing.stdout).split('\0') {
            // git still lists a file deleted from the working tree until the deletion is staged.
            let Ok(bytes) = read(&root.join(path)) else {
                continue;
            };
            if let Ok(text) = String::from_utf8(bytes) {
                files.push(File {
                    path: path.to_owned(),
                    text,
                });
            }
        }
        Ok(Self::new(files))
    }

    /// A repository made of `files`.
    pub fn new(files: Vec<File>) -> Self {
        Self { files }
    }

    /// Every file.
    pub fn files(&self) -> impl Iterator<Item = &File> {
        self.files.iter()
    }

    /// The file at `path`.
    pub fn file(&self, path: &str) -> Option<&File> {
        self.files.iter().find(|file| file.path == path)
    }

    /// Whether `path` is a file of the repository, or a directory that holds one.
    pub fn has(&self, path: &str) -> bool {
        let directory = format!("{}/", path.trim_end_matches('/'));
        path.is_empty()
            || self
                .files
                .iter()
                .any(|file| file.path == path || file.path.starts_with(&directory))
    }
}

#[cfg(test)]
impl Repo {
    /// A repository of `(path, text)` pairs.
    pub fn of(files: &[(&str, &str)]) -> Self {
        Self::new(
            files
                .iter()
                .map(|(path, text)| File {
                    path: (*path).to_owned(),
                    text: (*text).to_owned(),
                })
                .collect(),
        )
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the checks run as a synchronous program, with no async runtime to stall"
)]
fn read(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path)
}
