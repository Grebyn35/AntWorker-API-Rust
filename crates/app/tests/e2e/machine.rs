//! The machine that runs the end-to-end tests, which one run holds at a time: each run counts the
//! memory of the machine that runs Docker as its own (memory.rs). Before its first container
//! starts, a run removes what an earlier run left on the machine, as one stopped by hand leaves
//! its servers: every container that carries the label `LABEL`, with its anonymous volumes. It
//! then empties the registry that Compose runs, pushes the images under test there from target/,
//! and removes every other image pulled from it (OPS-21).

use std::{
    fs::{File, OpenOptions, TryLockError},
    sync::OnceLock,
};

use crate::{
    docker::{compose, docker},
    images::Pushed,
};

/// The label of every container that the tests start on the machine, by which a later run finds
/// what an earlier one left.
pub const LABEL: &str = "e2e-test-container";

/// Holds the machine for this run, from the first call on: a run that finds another holding it
/// fails, saying so, rather than counting the memory that the other's servers hold as its own.
/// The operating system lets the machine go when the run's process ends, however it ends.
pub fn claim() {
    static CLAIMED: OnceLock<File> = OnceLock::new();
    CLAIMED.get_or_init(|| {
        let lock = locked();
        let left = docker(&[
            "ps",
            "--all",
            "--quiet",
            "--filter",
            &format!("label={LABEL}"),
        ]);
        let left: Vec<&str> = left.lines().collect();
        if !left.is_empty() {
            docker(&[&["rm", "--force", "--volumes"][..], &left].concat());
        }
        push_images_under_test();
        remove_images_of_earlier_builds();
        lock
    });
}

/// The lock that one run holds at a time, on a file in the system's temporary directory, so runs
/// from different checkouts take turns too.
#[expect(
    clippy::disallowed_methods,
    reason = "tests may block, each on a runtime of its own (CODE-05)"
)]
fn locked() -> File {
    let path = std::env::temp_dir().join("e2e-tests.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .expect("the lock file in the temporary directory");
    let locked = file.try_lock();
    assert!(
        !matches!(locked, Err(TryLockError::WouldBlock)),
        "another run of the end-to-end tests holds this machine, as its lock on {} says: each \
         run counts the machine's memory as its own, so wait for that run to end, or end it",
        path.display()
    );
    locked.expect("the lock on the machine");
    file
}

/// Pushes the images that `just image` wrote to target/ to the registry that Compose runs, which
/// holds only them: a new container of the registry's starts with an empty store.
fn push_images_under_test() {
    compose(&["rm", "--force", "--stop", "--volumes", "registry"]);
    compose(&["up", "--detach", "--wait", "registry"]);
    let name = std::env::var("E2E_IMAGE_NAME").expect("E2E_IMAGE_NAME; run `just e2e`");
    for (layout, image) in [
        ("/image", name.clone()),
        ("/image-postgres", name + "-postgres"),
    ] {
        compose(&[
            "run",
            "--rm",
            "--no-TTY",
            "crane",
            "push",
            "--insecure",
            layout,
            &format!("registry:5000/{image}:e2e"),
        ]);
    }
}

/// Removes each image that a run pulled onto this machine from the registry that Compose runs,
/// but the images under test.
fn remove_images_of_earlier_builds() {
    let registry = compose(&["port", "registry", "5000"]);
    let current = Pushed::under_test().map(|image| image.on_host());
    let pulled = docker(&[
        "images",
        "--digests",
        "--filter",
        &format!("reference={registry}/*"),
        "--format",
        "{{.Repository}}@{{.Digest}}",
    ]);
    let earlier: Vec<&str> = pulled
        .lines()
        .filter(|image| !current.iter().any(|current| current == image))
        .collect();
    if !earlier.is_empty() {
        docker(&[&["image", "rm", "--force"][..], &earlier].concat());
    }
}
