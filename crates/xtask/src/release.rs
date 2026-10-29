//! `release`, which `just release <version>` runs: it sets the app's version on `main`, commits it
//! as the request to release it, and pushes it. The run on `main` that passes with that version
//! releases it (OPS-14). `release-check`, which that run calls first, fails on a version that
//! can't be released.
//!
//! The decisions are pure functions of what git reports, so tests cover them without a
//! repository; [`run`] gathers that state, and makes the change.

use std::{
    cmp::Ordering,
    fmt,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    str::FromStr,
};

/// The manifest that holds the app's version.
const MANIFEST: &str = "crates/app/Cargo.toml";
/// The lock file, which records the version too.
const LOCK: &str = "Cargo.lock";
/// The committed OpenAPI document, whose `info.version` is the app's version (API-17).
const OPENAPI: &str = "openapi.json";
/// The branch every release comes from.
const MAIN: &str = "main";

/// The subject of the commit that asks for the release of `version`. The `release-image` task in
/// the justfile looks for it among the commits since the latest release, so a version releases
/// only when this task asked for it.
fn request(version: Version) -> String {
    format!("Release {version}")
}

/// Whether a commit with `subject` asks for the release of `version`: the subject [`request`]
/// writes, or that subject with the number GitHub appends when it squash-merges a pull request,
/// such as "Release 1.4.0 (#12)". `release-image` in the justfile matches the same two forms.
fn asks_for(subject: &str, version: Version) -> bool {
    let Some(rest) = subject.strip_prefix(&request(version)) else {
        return false;
    };
    rest.is_empty()
        || rest
            .strip_prefix(" (#")
            .and_then(|rest| rest.strip_suffix(')'))
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

/// A release's version: `X.Y.Z`, or `X.Y.Z-rc.N` for a prerelease, as a tag `vX.Y.Z` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// The prerelease's number, for `-rc.N`.
    rc: Option<u64>,
}

impl Version {
    /// The next patch, minor and major versions after `self`. After a prerelease, the next patch
    /// is the release the prerelease led to.
    fn next(self) -> [Self; 3] {
        let final_ = |major, minor, patch| Self {
            major,
            minor,
            patch,
            rc: None,
        };
        let patch = match self.rc {
            Some(_) => final_(self.major, self.minor, self.patch),
            None => final_(self.major, self.minor, self.patch + 1),
        };
        [
            patch,
            final_(self.major, self.minor + 1, 0),
            final_(self.major + 1, 0, 0),
        ]
    }
}

impl FromStr for Version {
    type Err = ();

    /// Reads `X.Y.Z` or `X.Y.Z-rc.N`, with an optional leading `v`, and no leading zeros.
    fn from_str(text: &str) -> Result<Self, ()> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let (core, rc) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(number(pre.strip_prefix("rc.").ok_or(())?)?)),
            None => (text, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else {
            return Err(());
        };
        Ok(Self {
            major: number(major)?,
            minor: number(minor)?,
            patch: number(patch)?,
            rc,
        })
    }
}

fn number(text: &str) -> Result<u64, ()> {
    let leading_zero = text.len() > 1 && text.starts_with('0');
    if text.is_empty() || leading_zero || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    text.parse().map_err(|_| ())
}

impl Ord for Version {
    /// Semantic versioning's order: a prerelease comes before the release it leads to.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then(match (self.rc, other.rc) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(&b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match self.rc {
            Some(rc) => write!(f, "-rc.{rc}"),
            None => Ok(()),
        }
    }
}

/// What git reports about the repository, which decides whether a release may start.
#[derive(Debug)]
pub struct State {
    /// Files with changes that aren't committed.
    pub changed: Vec<String>,
    /// The branch checked out.
    pub branch: String,
    /// The commits on this `main` that GitHub's lacks, which the release's push takes with it,
    /// so that the release's run checks them.
    pub ahead: u32,
    /// The commits on GitHub's `main` that this one lacks.
    pub behind: u32,
    /// The versions of every release tag.
    pub releases: Vec<Version>,
    /// The version `main` holds now.
    pub current: Version,
    /// Whether a commit since the latest release asks for the release of `current`, which the
    /// first run on `main` that passes then makes.
    pub pending: bool,
    /// The commits after the latest release, if there is one.
    pub commits_since_latest: Option<u32>,
}

/// Why `requested` can't be released, with what to do, or `None` if it can be.
pub fn refusal(state: &State, requested: Version) -> Option<String> {
    if !state.changed.is_empty() {
        return Some(format!(
            "There are changes that aren't committed: {}. Commit them, or set them aside with \
             `git stash`, and run this again.",
            state.changed.join(", ")
        ));
    }
    if state.branch != MAIN {
        return Some(format!(
            "A release comes from {MAIN}, and {} is checked out. Switch with `git switch {MAIN}`, \
             and run this again.",
            state.branch
        ));
    }
    if state.behind > 0 {
        return Some(format!(
            "GitHub's {MAIN} has commits that this one doesn't. Get them with `git pull`, and run \
             this again."
        ));
    }
    if let Some(refused) = version_refusal(&state.releases, requested) {
        return Some(refused);
    }
    let latest = state.releases.iter().max();
    if let (Some(latest), Some(0)) = (latest, state.commits_since_latest) {
        return Some(format!("Nothing has changed since v{latest}."));
    }
    if state.pending && state.current == requested {
        return Some(format!(
            "{MAIN} is set to {requested} already, and the first run on {MAIN} that passes \
             releases it. If a run failed, fix what failed and push the fix: its run releases \
             {requested}."
        ));
    }
    None
}

/// Why `requested` can't be a new release after `releases`, with the versions that can, or
/// `None` if it can be.
pub fn version_refusal(releases: &[Version], requested: Version) -> Option<String> {
    let latest = releases.iter().max()?;
    let refused = if releases.contains(&requested) {
        format!("{requested} is released already, as v{requested}.")
    } else if requested < *latest {
        format!("{requested} comes before the latest release, {latest}.")
    } else {
        return None;
    };
    let [patch, minor, major] = latest.next();
    Some(format!(
        "{refused} Release {patch} for fixes, {minor} for new features that keep everything \
         working, or {major} for changes that make forks or clients change something: run \
         `just release {patch}`, `just release {minor}` or `just release {major}`."
    ))
}

/// Replaces the app's version `old` with `new` in the text of `path`, or says why it can't.
pub fn set_version(path: &str, text: &str, old: Version, new: Version) -> Result<String, String> {
    let (from, to) = match path {
        MANIFEST => (
            format!("\nversion = \"{old}\"\n"),
            format!("\nversion = \"{new}\"\n"),
        ),
        LOCK => (
            format!("\nname = \"app\"\nversion = \"{old}\"\n"),
            format!("\nname = \"app\"\nversion = \"{new}\"\n"),
        ),
        _ => (
            format!("\"version\": \"{old}\""),
            format!("\"version\": \"{new}\""),
        ),
    };
    if !text.contains(&from) {
        return Err(format!(
            "{path} doesn't hold the version {old} that {MANIFEST} names. Run `just openapi` \
             and `cargo metadata`, commit what changes, and run this again."
        ));
    }
    Ok(text.replacen(&from, &to, 1))
}

/// Releases `requested`: checks that it can, sets the version, commits, and pushes `main`.
pub fn run(requested: &str) -> ExitCode {
    let Ok(requested) = requested.parse::<Version>() else {
        return refuse(&format!(
            "{requested} isn't a version. A version is X.Y.Z, such as 1.4.0, or X.Y.Z-rc.N for a \
             prerelease, such as 1.4.0-rc.1."
        ));
    };
    let root = root();
    let git = Git { root: &root };
    if let Err(error) = git.run(&["fetch", "--quiet", "--tags", "origin", MAIN]) {
        return refuse(&format!("Can't reach GitHub to compare {MAIN}: {error}"));
    }
    let state = match git.state(&root) {
        Ok(state) => state,
        Err(error) => return refuse(&error),
    };
    if let Some(refused) = refusal(&state, requested) {
        return refuse(&refused);
    }
    if let Err(error) = write_version(&root, state.current, requested) {
        return refuse(&error);
    }
    let message = request(requested);
    // A version that main holds without a request, as in a repository made from the template,
    // changes no file, and its commit is the request alone.
    let committed = git
        .run(&["add", MANIFEST, LOCK, OPENAPI])
        .and_then(|_| git.run(&["commit", "--quiet", "--allow-empty", "--message", &message]));
    if let Err(error) = committed {
        return refuse(&format!("Can't commit the version: {error}"));
    }
    if let Err(error) = git.run(&["push", "--quiet", "origin", MAIN]) {
        return refuse(&format!(
            "GitHub refused the push to {MAIN}: {error}\nIf {MAIN} takes changes only through \
             pull requests, push {MAIN}'s new commits to a branch with `git push origin \
             {MAIN}:release-{requested}`, open a pull request from it, and merge it: its run \
             on {MAIN} releases {requested}. Then run `git reset --hard origin/{MAIN}`."
        ));
    }
    let pushed = match state.ahead {
        0 => format!("\"{message}\""),
        1 => format!("your commit and \"{message}\""),
        ahead => format!("your {ahead} commits and \"{message}\""),
    };
    report(&format!(
        "Pushed {pushed} to {MAIN}. The run on {MAIN} releases {requested} once this commit \
         passes every check, in about 20 minutes: follow it on the repository's Actions page on \
         GitHub. docs/operations/release.md says what to do if it fails."
    ))
}

/// Fails unless `version` can be a new release after the repository's release tags. The run on
/// `main` calls it before it releases a version.
pub fn check(version: &str) -> ExitCode {
    let Ok(version) = version.parse::<Version>() else {
        return refuse(&format!(
            "{MANIFEST} sets the version {version}, which no release can have: set X.Y.Z or \
             X.Y.Z-rc.N with `just release`."
        ));
    };
    let root = root();
    match (Git { root: &root }).releases() {
        Ok(releases) => match version_refusal(&releases, version) {
            Some(refused) => refuse(&format!(
                "{MANIFEST} sets {version}, so this commit isn't released. {refused}"
            )),
            None => ExitCode::SUCCESS,
        },
        Err(error) => refuse(&error),
    }
}

/// The latest version that the repository's release tags name, or none before the first release.
pub fn latest_release(root: &Path) -> Result<Option<Version>, String> {
    Ok((Git { root }).releases()?.into_iter().max())
}

/// Writes `new` in place of `old` in each file that holds the app's version, changing none of
/// them unless every one can be changed.
fn write_version(root: &Path, old: Version, new: Version) -> Result<(), String> {
    let mut written = Vec::new();
    for path in [MANIFEST, LOCK, OPENAPI] {
        let text = read(&root.join(path)).map_err(|error| format!("Can't read {path}: {error}"))?;
        written.push((path, set_version(path, &text, old, new)?));
    }
    for (path, text) in written {
        write(&root.join(path), &text).map_err(|error| format!("Can't write {path}: {error}"))?;
    }
    Ok(())
}

/// git, run in the repository.
struct Git<'a> {
    root: &'a Path,
}

impl Git<'_> {
    /// Runs git with `args`, and returns what it wrote on stdout, or on stderr if it failed.
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(self.root)
            .output()
            .map_err(|error| format!("can't run git: {error}"))?;
        if output.status.success() {
            // Only the end: `git status --porcelain` begins a line with a space.
            Ok(String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    /// The versions of the release tags.
    fn releases(&self) -> Result<Vec<Version>, String> {
        Ok(self
            .run(&["tag", "--list", "v*"])?
            .lines()
            .filter_map(|tag| tag.parse().ok())
            .collect())
    }

    fn state(&self, root: &Path) -> Result<State, String> {
        let changed = changed_files(&self.run(&["status", "--porcelain"])?);
        let branch = self.run(&["rev-parse", "--abbrev-ref", "HEAD"])?;
        let counts = self.run(&[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...origin/{MAIN}"),
        ])?;
        let mut counts = counts.split_whitespace().map(str::parse::<u32>);
        let (Some(Ok(ahead)), Some(Ok(behind))) = (counts.next(), counts.next()) else {
            return Err(format!("git can't compare {MAIN} with GitHub's"));
        };
        let releases = self.releases()?;
        let since = releases
            .iter()
            .max()
            .map_or_else(|| "HEAD".to_owned(), |latest| format!("v{latest}..HEAD"));
        let subjects = self.run(&["log", "--format=%s", &since])?;
        let commits_since_latest = releases
            .iter()
            .max()
            .map(|_| u32::try_from(subjects.lines().count()).unwrap_or(u32::MAX));
        let manifest = read(&root.join(MANIFEST))
            .map_err(|error| format!("Can't read {MANIFEST}: {error}"))?;
        let current = manifest
            .lines()
            .find_map(|line| line.strip_prefix("version = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .and_then(|version| version.parse().ok())
            .ok_or(format!("{MANIFEST} has no version that a release can have"))?;
        let pending = subjects.lines().any(|subject| asks_for(subject, current));
        Ok(State {
            changed,
            branch,
            ahead,
            behind,
            releases,
            current,
            pending,
            commits_since_latest,
        })
    }
}

/// The paths in `git status --porcelain`'s output, each line of which is two status letters, a
/// space and a path.
fn changed_files(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter_map(|line| line.get(3..))
        .map(str::to_owned)
        .collect()
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[expect(
    clippy::disallowed_methods,
    reason = "the task runs as a synchronous program, with no async runtime to stall"
)]
fn read(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

#[expect(
    clippy::disallowed_methods,
    reason = "the task runs as a synchronous program, with no async runtime to stall"
)]
fn write(path: &Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)
}

#[expect(clippy::print_stderr, reason = "the refusal is the task's output")]
fn refuse(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[expect(clippy::print_stdout, reason = "the report is the task's output")]
fn report(message: &str) -> ExitCode {
    println!("{message}");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    /// A repository ready to release: clean, on main, in step with GitHub, with releases
    /// 1.0.0 and 1.1.0, and three commits since the latter.
    fn ready() -> State {
        State {
            changed: Vec::new(),
            branch: "main".to_owned(),
            ahead: 0,
            behind: 0,
            releases: vec![v("1.0.0"), v("1.1.0")],
            current: v("1.1.0"),
            pending: false,
            commits_since_latest: Some(3),
        }
    }

    #[test]
    fn ops_14_a_version_is_x_y_z_or_a_numbered_prerelease() {
        assert_eq!(v("1.4.0").to_string(), "1.4.0");
        assert_eq!(v("v1.4.0-rc.2").to_string(), "1.4.0-rc.2");
        for invalid in [
            "1.4",
            "1.4.0.1",
            "01.4.0",
            "1.4.0-beta.1",
            "1.4.0-rc",
            "1.x.0",
            "",
        ] {
            assert!(invalid.parse::<Version>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn ops_14_a_prerelease_comes_before_its_release() {
        let mut versions = [v("1.0.0"), v("1.0.0-rc.10"), v("0.9.9"), v("1.0.0-rc.2")];
        versions.sort();
        let sorted: Vec<String> = versions.iter().map(ToString::to_string).collect();

        assert_eq!(sorted, ["0.9.9", "1.0.0-rc.2", "1.0.0-rc.10", "1.0.0"]);
    }

    #[test]
    fn ops_14_a_release_starts_only_from_a_clean_main_that_holds_github_s_commits() {
        type Change = fn(&mut State);
        let cases: [(Change, &str); 3] = [
            (
                |state| state.changed = vec!["justfile".to_owned()],
                "git stash",
            ),
            (
                |state| state.branch = "feature".to_owned(),
                "git switch main",
            ),
            (|state| state.behind = 2, "git pull"),
        ];
        for (change, next_step) in cases {
            let mut state = ready();
            change(&mut state);

            let refused = refusal(&state, v("1.2.0")).unwrap();

            assert!(refused.contains(next_step), "{refused}");
        }
        assert_eq!(refusal(&ready(), v("1.2.0")), None);
    }

    #[test]
    fn ops_14_commits_that_github_lacks_go_out_with_the_release_in_one_run() {
        let state = State {
            ahead: 2,
            ..ready()
        };

        assert_eq!(refusal(&state, v("1.2.0")), None);
    }

    #[test]
    fn ops_14_the_refusal_names_each_file_that_isn_t_committed() {
        assert_eq!(
            changed_files(" M README.md\nM  justfile\n?? notes.txt"),
            ["README.md", "justfile", "notes.txt"]
        );
    }

    #[test]
    fn ops_14_a_version_released_or_below_the_latest_is_refused_with_the_next_ones() {
        for requested in ["1.1.0", "1.0.5", "1.1.0-rc.1"] {
            let refused = refusal(&ready(), v(requested)).unwrap();

            assert!(
                refused.contains("`just release 1.1.1`")
                    && refused.contains("`just release 1.2.0`")
                    && refused.contains("`just release 2.0.0`"),
                "{refused}"
            );
        }
    }

    #[test]
    fn ops_14_after_a_prerelease_the_next_patch_is_its_release() {
        let refused = version_refusal(&[v("2.0.0-rc.1")], v("1.9.0")).unwrap();

        assert!(refused.contains("`just release 2.0.0`"), "{refused}");
        assert!(refused.contains("`just release 2.1.0`"), "{refused}");
        assert_eq!(version_refusal(&[v("2.0.0-rc.1")], v("2.0.0-rc.2")), None);
    }

    #[test]
    fn ops_14_a_release_needs_a_change_since_the_latest() {
        let state = State {
            commits_since_latest: Some(0),
            ..ready()
        };

        assert_eq!(
            refusal(&state, v("1.1.1")).as_deref(),
            Some("Nothing has changed since v1.1.0.")
        );
    }

    #[test]
    fn ops_14_a_version_waiting_for_its_run_is_not_asked_for_again() {
        let state = State {
            current: v("1.2.0"),
            pending: true,
            ..ready()
        };

        let refused = refusal(&state, v("1.2.0")).unwrap();

        assert!(refused.contains("set to 1.2.0 already"), "{refused}");
        assert_eq!(refusal(&state, v("1.3.0")), None);
    }

    #[test]
    fn ops_14_a_version_that_no_release_commit_asked_for_can_be_released() {
        // A repository made from the template holds the template's version, and none of its tags.
        let state = State {
            releases: Vec::new(),
            current: v("1.3.0"),
            pending: false,
            commits_since_latest: None,
            ..ready()
        };

        assert_eq!(refusal(&state, v("1.3.0")), None);
        assert_eq!(request(v("1.3.0")), "Release 1.3.0");
    }

    #[test]
    fn ops_14_a_request_merged_through_a_pull_request_still_asks_for_its_release() {
        let version = v("1.4.0");

        assert!(asks_for("Release 1.4.0", version));
        assert!(asks_for("Release 1.4.0 (#12)", version));
        for other in [
            "Release 1.4.0-rc.1",
            "Release 1.4.0 (#)",
            "Release 1.4.0 (#12) and more",
            "Release 1.4.01",
            "Prepare the release 1.4.0",
        ] {
            assert!(!asks_for(other, version), "{other}");
        }
    }

    #[test]
    fn ops_14_the_first_release_can_be_any_version() {
        let state = State {
            releases: Vec::new(),
            current: v("0.1.0"),
            commits_since_latest: None,
            ..ready()
        };

        assert_eq!(refusal(&state, v("1.0.0")), None);
    }

    #[test]
    fn ops_14_the_version_changes_in_the_manifest_the_lock_file_and_openapi_json() {
        let (old, new) = (v("0.1.0"), v("1.0.0"));
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition.workspace = true\n";
        let lock = "[[package]]\nname = \"ahash\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n";
        let openapi =
            "{\n  \"info\": {\n    \"title\": \"api\",\n    \"version\": \"0.1.0\"\n  }\n}\n";

        assert_eq!(
            set_version(MANIFEST, manifest, old, new).unwrap(),
            manifest.replace("0.1.0", "1.0.0")
        );
        assert_eq!(
            set_version(LOCK, lock, old, new).unwrap(),
            "[[package]]\nname = \"ahash\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"app\"\nversion = \"1.0.0\"\n"
        );
        assert_eq!(
            set_version(OPENAPI, openapi, old, new).unwrap(),
            openapi.replace("0.1.0", "1.0.0")
        );
        assert!(
            set_version(OPENAPI, "{}", old, new)
                .unwrap_err()
                .contains("just openapi")
        );
    }
}
