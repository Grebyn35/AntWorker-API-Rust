//! The routes that each test sends a request to, written for `just test`, which reads them
//! against the specs' Routes tables once the tests pass (TEST-01).
//!
//! A test's API records each request that one of its routes takes, as the name of the test's
//! thread, the request's method and the route's path, as axum matched it, such as
//! `/v1/workspaces/{workspace_id}`. The thread is the one that builds the API, which the test
//! framework names after the test when it runs the tests on more than one thread, and `main`
//! when it runs them on one. A request that matches no route is recorded nowhere.
//!
//! The lines go to the file that `SENT_ROUTES` names, which the first request of the process
//! empties. A run without the variable, such as `cargo test` by hand, records nothing.

use std::{
    collections::HashSet,
    fs::File,
    io::Write,
    sync::{Mutex, OnceLock, PoisonError},
};

use axum::{
    Router,
    extract::{MatchedPath, Request},
    middleware::{self, Next},
};

/// The environment variable that names the file of the record.
const FILE: &str = "SENT_ROUTES";

/// The record of the process, once its first request opened it, or `None` without the variable.
static RECORD: OnceLock<Option<Mutex<Record>>> = OnceLock::new();

/// A record: its file, and the lines that the file holds.
struct Record {
    file: File,
    lines: HashSet<String>,
}

/// `router`, which records each request that one of its routes takes, for the test whose thread
/// calls this, when `SENT_ROUTES` names a file.
pub(super) fn recording(router: Router) -> Router {
    if std::env::var_os(FILE).is_none() {
        return router;
    }
    let test = std::thread::current().name().unwrap_or_default().to_owned();
    router.layer(middleware::from_fn(move |request: Request, next: Next| {
        if let Some(route) = request.extensions().get::<MatchedPath>() {
            record(format!("{test}\t{}\t{}", request.method(), route.as_str()));
        }
        next.run(request)
    }))
}

/// Writes `line` to the record, once.
#[expect(
    clippy::disallowed_methods,
    reason = "the test support may block: it writes a line of a local file once for each route \
              that a test sends to"
)]
fn record(line: String) {
    let record = RECORD.get_or_init(|| {
        let path = std::env::var_os(FILE)?;
        if let Some(directory) = std::path::Path::new(&path).parent() {
            std::fs::create_dir_all(directory).expect("the directory of SENT_ROUTES");
        }
        let file = File::create(&path).expect("the file that SENT_ROUTES names");
        let lines = HashSet::new();
        Some(Mutex::new(Record { file, lines }))
    });
    let Some(record) = record else {
        return;
    };
    let mut record = record.lock().unwrap_or_else(PoisonError::into_inner);
    if record.lines.insert(line.clone()) {
        writeln!(record.file, "{line}").expect("a line of SENT_ROUTES");
    }
}
