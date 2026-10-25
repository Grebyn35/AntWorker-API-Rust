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
        Self::from_listing(&listing.stdout, |path| read(&root.join(path)))
    }

    /// The text files among `listing`, git's list of paths separated by NUL bytes, each read
    /// with `read`.
    fn from_listing(
        listing: &[u8],
        read: impl Fn(&str) -> io::Result<Vec<u8>>,
    ) -> io::Result<Self> {
        let mut files = Vec::new();
        let listing = String::from_utf8_lossy(listing);
        for path in listing.split('\0').filter(|path| !path.is_empty()) {
            let bytes = match read(path) {
                Ok(bytes) => bytes,
                // git still lists a file deleted from the working tree until the deletion is
                // staged.
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io::Error::new(error.kind(), format!("{path}: {error}"))),
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

/// Writes `text` to the file at `path`, a path from the repository root, with line feeds.
#[expect(
    clippy::disallowed_methods,
    reason = "the checks run as a synchronous program, with no async runtime to stall"
)]
pub fn write(path: &str, text: &str) -> io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::write(root.join(path), text)
}

/// Makes the directory at `path`, a path from the repository root, empty: it removes what the
/// directory holds, or creates it.
#[expect(
    clippy::disallowed_methods,
    reason = "the checks run as a synchronous program, with no async runtime to stall"
)]
pub fn empty_directory(path: &str) -> io::Result<()> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    match std::fs::remove_dir_all(&directory) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    std::fs::create_dir_all(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_13_a_listed_file_that_cant_be_read_fails_the_checks() {
        let read = |path: &str| match path {
            "gone.md" => Err(io::Error::from(io::ErrorKind::NotFound)),
            "locked.md" => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            _ => Ok(b"# Text\n".to_vec()),
        };

        let deleted = Repo::from_listing(b"a.md\0gone.md\0", read).unwrap();
        let unreadable = Repo::from_listing(b"a.md\0locked.md\0", read).err();

        let paths: Vec<&str> = deleted.files().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["a.md"]);
        assert!(
            unreadable.is_some_and(|error| error.to_string().contains("locked.md")),
            "a file that exists but can't be read must fail the checks"
        );
    }
}
