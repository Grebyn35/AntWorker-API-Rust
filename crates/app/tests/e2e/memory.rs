//! The memory of the machine that runs Docker, in which the servers keep their Docker stores
//! (D84), so the suite writes next to nothing to that machine's disk. Each server holds a share
//! of it, under a limit that Docker enforces, and the shares fit in what the machine had
//! available when the first server started, beside the reserve of the heavy turn: the turn of a
//! deployment whose telemetry runs on its server, the self-hosted stack or a stand-in for a
//! managed Grafana, whose images are the largest. One deployment holds that turn at a time, in
//! addition to its share. A test takes every share it needs at once, and the heavy turn only
//! while it holds no other turn but its share, so no test holds a turn while it waits for one
//! that another test holds. Where the machine can't hold one share beside the reserve, every
//! server keeps its store on disk, and the heavy turn still goes one at a time, which bounds the
//! disk the suite takes.

use std::sync::{Condvar, Mutex, OnceLock, PoisonError};

use tokio::sync::Semaphore;

use crate::docker::{compose, docker};

/// What a server may hold in memory, its store and its processes together, in mebibytes: a
/// bundled database's server that backs up and restores holds up to 2.3 gibibytes. The limit is
/// Docker's, so a server that would take more ends, and its test fails, while the machine keeps
/// the rest.
pub const SHARE_MIB: u64 = 2_560;
/// What the server that holds the heavy turn may hold beyond its share: a bundled database's
/// server with the self-hosted stack holds up to 6.1 gibibytes.
pub const HEAVY_MIB: u64 = 4_608;
/// What the machine keeps beside the servers: for the managed databases' servers, which keep
/// their data in memory too, and the tests' own processes.
const HEADROOM_MIB: u64 = 2_048;

/// The shares of the servers' memory, or none where the servers keep their stores on disk.
pub fn shares() -> Option<&'static Turns> {
    static SHARES: OnceLock<Option<Turns>> = OnceLock::new();
    SHARES
        .get_or_init(|| {
            let shares = available_mib().saturating_sub(HEAVY_MIB + HEADROOM_MIB) / SHARE_MIB;
            (shares > 0).then(|| Turns::new(shares))
        })
        .as_ref()
}

/// The heavy turn, which one deployment holds at a time.
pub static HEAVY: Semaphore = Semaphore::const_new(1);

/// The memory that the machine running Docker has available, in mebibytes, as a container
/// reads it there.
fn available_mib() -> u64 {
    let image = compose(&["config", "--images", "server"]);
    let meminfo = docker(&[
        "run",
        "--rm",
        "--entrypoint",
        "cat",
        &image,
        "/proc/meminfo",
    ]);
    let kib: u64 = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|rest| rest.split_whitespace().next())
        .expect("MemAvailable in /proc/meminfo")
        .parse()
        .expect("a number of kibibytes");
    kib / 1024
}

/// Turns that threads wait for, as a semaphore counts them: a server starts on its test's own
/// thread, outside any task that a runtime could wake. A thread that waits for several takes
/// them all at once, and the turns go in the order the threads asked for them.
pub struct Turns {
    queue: Mutex<Queue>,
    changed: Condvar,
}

struct Queue {
    free: u64,
    /// The ticket that the next thread to ask gets.
    next: u64,
    /// The ticket of the thread whose turn is next.
    serving: u64,
}

impl Turns {
    fn new(count: u64) -> Self {
        Self {
            queue: Mutex::new(Queue {
                free: count,
                next: 0,
                serving: 0,
            }),
            changed: Condvar::new(),
        }
    }

    /// `count` turns, once they are free and every thread that asked earlier has its own.
    pub fn take(&'static self, count: u64) -> Turn {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let ticket = queue.next;
        queue.next += 1;
        while queue.serving != ticket || queue.free < count {
            queue = self
                .changed
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
        queue.free -= count;
        queue.serving += 1;
        self.changed.notify_all();
        Turn { turns: self, count }
    }
}

/// Turns, given back when they drop.
pub struct Turn {
    turns: &'static Turns,
    count: u64,
}

impl Turn {
    /// One of these turns, as a turn of its own, and the rest.
    pub fn split_one(mut self) -> (Self, Self) {
        assert!(self.count > 1, "turns to split");
        self.count -= 1;
        let one = Self {
            turns: self.turns,
            count: 1,
        };
        (one, self)
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let mut queue = self
            .turns
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        queue.free += self.count;
        self.turns.changed.notify_all();
    }
}
