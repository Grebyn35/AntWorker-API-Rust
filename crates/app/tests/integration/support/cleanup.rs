//! Dropping what a test leaves on the server: its database, and the roles it created (OPS-21).
// part: files
//! Its bucket goes with them.
// end part: files
//!
//! One thread of the process drops them, on a runtime of its own, as each test's `TestDatabase`
//! goes. The test never waits for it: `DROP DATABASE` waits for every backend of the server, one
//! that is still starting among them, such as the connection that the test's API was opening as
//! the test ended, which only the test's own runtime ends, once it goes, after the test's values.
//! A test that waited for the statement would hold it, and every other test's, until Postgres
//! gives the starting backend up, a minute later.
//!
//! The process ends without waiting for the thread, so what its last tests left may stay. The
//! next process drops it first: each process holds an advisory lock that its ID names while it
//! runs, so what a process left, a killed one too, is whatever its ID names while its lock is
//! free.

use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

use platform::db::DatabaseUrl;
use sqlx::PgConnection;
use tokio::sync::{OnceCell, mpsc, oneshot};

use super::{close, connect, execute, identifier};

/// The class of the advisory locks that the test processes hold while they run, each with its
/// process's ID.
const RUNNING: i32 = 730_015;

/// What a test leaves on the server once its `TestDatabase` goes.
pub(super) struct Leftover {
    /// Its database.
    pub(super) database: String,
    /// The roles it created.
    pub(super) roles: Vec<String>,
    // part: files
    /// Its bucket, if it created one.
    pub(super) bucket: Option<String>,
    // end part: files
}

/// Where the tests' leftovers go, once [`start`] has started the thread that drops them.
static LEFT: OnceCell<mpsc::UnboundedSender<Leftover>> = OnceCell::const_new();

/// Why the thread that drops the leftovers stopped, if it did.
static FAILED: Mutex<Option<String>> = Mutex::new(None);

/// Starts the thread that drops what the tests of this process leave, once it holds the lock
/// that says the process runs, and has dropped what the processes that run no more left. The
/// caller holds the tests' setup lock, so no other process starts, or creates a database,
/// meanwhile.
pub(super) async fn start(admin: &DatabaseUrl) {
    LEFT.get_or_init(|| async {
        let (left, leftovers) = mpsc::unbounded_channel();
        let (ready, started) = oneshot::channel();
        let admin = admin.clone();
        std::thread::spawn(move || {
            let stopped = Stopped;
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the cleanup's runtime")
                .block_on(run(&admin, ready, leftovers));
            std::mem::forget(stopped);
        });
        started.await.expect("the cleanup thread starts");
        left
    })
    .await;
}

/// Hands `leftover` to the thread that drops it, without waiting for it.
pub(super) fn leave(leftover: Leftover) {
    if let Some(left) = LEFT.get() {
        // The thread is gone only once it failed, which the next test reports.
        left.send(leftover).unwrap_or_default();
    }
}

/// Fails the test that calls it once the thread that drops the leftovers has stopped, so a
/// cleanup that fails shows in the tests that follow it.
pub(super) fn check() {
    let failed = FAILED.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(
        failed.is_none(),
        "the cleanup of the tests' databases stopped: {}",
        failed.as_deref().unwrap_or_default()
    );
}

/// Records that the cleanup thread stopped by a panic, when dropped during one.
struct Stopped;

impl Drop for Stopped {
    fn drop(&mut self) {
        *FAILED.lock().unwrap_or_else(PoisonError::into_inner) =
            Some("its thread panicked, as the test output above shows".to_owned());
    }
}

/// Holds the lock that says this process runs, drops what the processes that run no more left,
/// says it is `ready`, then drops each of `leftovers` as it comes.
async fn run(
    url: &DatabaseUrl,
    ready: oneshot::Sender<()>,
    mut leftovers: mpsc::UnboundedReceiver<Leftover>,
) {
    let mut admin = connect(url).await;
    let process = i32::try_from(std::process::id()).expect("a process ID that an int4 holds");
    execute(
        &mut admin,
        &format!("SELECT pg_advisory_lock({RUNNING}, {process})"),
    )
    .await;
    drop_left_by_others(url).await;
    // The test that started the thread may have failed meanwhile.
    ready.send(()).unwrap_or_default();
    while let Some(leftover) = leftovers.recv().await {
        remove(&mut admin, leftover).await;
    }
}

/// Drops what the test processes that run no more left, on a connection of its own.
pub(super) async fn drop_left_by_others(url: &DatabaseUrl) {
    let mut admin = connect(url).await;
    let process = i32::try_from(std::process::id()).expect("a process ID that an int4 holds");
    for leftover in left_by_others(&mut admin, url.database(), process).await {
        remove(&mut admin, leftover).await;
    }
    close(admin).await;
}

/// Drops `leftover`'s database, with the connections it still has, and its roles.
async fn remove(admin: &mut PgConnection, leftover: Leftover) {
    // Each statement goes alone: DROP DATABASE can't run in the implicit transaction that a
    // batch of statements shares.
    execute(
        admin,
        &format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            identifier(&leftover.database)
        ),
    )
    .await;
    for role in &leftover.roles {
        execute(admin, &format!("DROP ROLE IF EXISTS {}", identifier(role))).await;
    }
    // part: files
    if let Some(bucket) = &leftover.bucket {
        super::bucket::remove(bucket).await;
    }
    // end part: files
}

/// What the test processes that run no more left: a leftover for each database that the name of
/// a process other than `process` holds, whose lock is free, and one for their roles. `prefix`
/// starts the tests' names, such as `api` in `api_test_4120_7`.
async fn left_by_others(admin: &mut PgConnection, prefix: &str, process: i32) -> Vec<Leftover> {
    let databases = sqlx::query_scalar!(
        r#"SELECT datname AS "name!" FROM pg_database WHERE datname ~ $1"#,
        format!("^{prefix}_test_[0-9]+_[0-9]+$")
    )
    .fetch_all(&mut *admin)
    .await
    .expect("list the test databases");
    let roles = sqlx::query_scalar!(
        r#"SELECT rolname AS "name!" FROM pg_roles WHERE rolname ~ $1"#,
        format!("^{prefix}_role_[0-9]+_[0-9]+$")
    )
    .fetch_all(&mut *admin)
    .await
    .expect("list the tests' roles");
    let mut running: HashMap<i32, bool> = HashMap::from([(process, true)]);
    let mut left = Vec::new();
    for database in databases {
        if !runs(admin, &mut running, &database).await {
            left.push(Leftover {
                // part: files
                bucket: existing_bucket(&database).await,
                // end part: files
                database,
                roles: Vec::new(),
            });
        }
    }
    for role in roles {
        if !runs(admin, &mut running, &role).await {
            // A role goes once the databases that it may own objects in have gone, with a
            // database's name that none has, whose drop does nothing.
            left.push(Leftover {
                database: format!("{prefix}_test_0_0"),
                roles: vec![role],
                // part: files
                bucket: None,
                // end part: files
            });
        }
    }
    left
}

// part: files
/// The bucket of the test whose database is `database`, if the storage still holds it.
async fn existing_bucket(database: &str) -> Option<String> {
    let bucket = super::bucket::name(database);
    super::bucket::exists(&bucket).await.then_some(bucket)
}
// end part: files

/// Whether the process that the name `name` holds, between its kind and its number, as
/// `api_test_4120_7` holds 4120, still runs: its lock is taken. A name that holds no process ID
/// counts as a running process's, and stays.
async fn runs(admin: &mut PgConnection, running: &mut HashMap<i32, bool>, name: &str) -> bool {
    let Some(process) = name
        .rsplit('_')
        .nth(1)
        .and_then(|process| process.parse::<i32>().ok())
    else {
        return true;
    };
    if let Some(known) = running.get(&process) {
        return *known;
    }
    let free = sqlx::query_scalar!(
        r#"SELECT pg_try_advisory_lock($1, $2) AS "free!""#,
        RUNNING,
        process
    )
    .fetch_one(&mut *admin)
    .await
    .expect("try the lock of a process's ID");
    if free {
        execute(
            admin,
            &format!("SELECT pg_advisory_unlock({RUNNING}, {process})"),
        )
        .await;
    }
    running.insert(process, !free);
    !free
}
