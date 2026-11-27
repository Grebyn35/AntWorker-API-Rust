//! The lines that the tests' logger writes on a test's thread, each with the status that the
//! readiness probe answered as the line was written, for the tests of a line that follows a
//! change of readiness (PLATFORM-061).

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, ThreadId},
};

use serde_json::Value;

use super::LOGGED;
use crate::{errors::Held, lifecycle::Readiness};

/// A line that a [`LinesWithReadiness`] kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Its level, such as `WARN`.
    pub level: String,
    /// Its message.
    pub message: String,
    /// The status that the readiness probe answered as the line was written.
    pub readiness: &'static str,
}

/// Numbers the keepers.
static NEXT_KEEPER: AtomicU64 = AtomicU64::new(0);

/// What a [`LinesWithReadiness`] reads, on which thread, and where it keeps the lines. A lock
/// that a panic poisoned still holds whole lines, since each change of them is one push, so it is
/// taken as it is.
pub(super) struct Keeper {
    keeper: u64,
    thread: ThreadId,
    readiness: Readiness,
    written: Arc<Mutex<Vec<Line>>>,
}

/// Keeps each line that the tests' logger writes on the thread that creates it, which is the
/// test's, until it is dropped, with the status that `readiness` answers as the line is written.
/// The logger keeps and prints the lines as it does every line.
#[must_use = "the lines are kept until it is dropped"]
pub struct LinesWithReadiness {
    keeper: u64,
    written: Arc<Mutex<Vec<Line>>>,
}

impl LinesWithReadiness {
    /// Keeps the lines of this thread, each with what `readiness` answers as it is written.
    pub fn of(readiness: &Readiness) -> Self {
        let keeper = NEXT_KEEPER.fetch_add(1, Ordering::Relaxed);
        let written = Arc::default();
        LOGGED.lock().held().keepers.push(Keeper {
            keeper,
            thread: thread::current().id(),
            readiness: readiness.clone(),
            written: Arc::clone(&written),
        });
        Self { keeper, written }
    }

    /// The lines kept so far, in the order they were written.
    pub fn written(&self) -> Vec<Line> {
        self.written.lock().held().clone()
    }
}

impl Drop for LinesWithReadiness {
    fn drop(&mut self) {
        LOGGED
            .lock()
            .held()
            .keepers
            .retain(|keeper| keeper.keeper != self.keeper);
    }
}

/// Gives the lines of `written`, a write of the tests' logger on `thread`, to each of `keepers`
/// that keeps that thread's.
pub(super) fn keep(keepers: &[Keeper], thread: ThreadId, written: &str) {
    let ours: Vec<&Keeper> = keepers
        .iter()
        .filter(|keeper| keeper.thread == thread)
        .collect();
    if ours.is_empty() {
        return;
    }
    for line in written
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let text = |field: &str| line[field].as_str().unwrap_or_default().to_owned();
        for keeper in &ours {
            keeper.written.lock().held().push(Line {
                level: text("level"),
                message: text("message"),
                readiness: keeper.readiness.status(),
            });
        }
    }
}
