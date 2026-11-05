//! The memory of the machine that runs Docker, in which the servers keep their Docker stores, and
//! the managed databases' servers their data (D84), so the suite writes next to nothing to that
//! machine's disk. Each server holds a share of it: its own limit and its managed database's
//! server's, which Docker enforces. The shares fit in what the machine had available when the
//! first server started, beside the reserve of the heavy turn: the turn of a deployment whose
//! telemetry runs on its server, the self-hosted stack or a stand-in for a managed Grafana, whose
//! images are the largest. One deployment holds that turn at a time, in addition to its share. A
//! test takes every share it needs at once, before any other turn, and then the heavy turn, if it
//! needs it, holding only its share: so the only test that waits while it holds a turn waits for
//! the heavy turn, whose holder waits for nothing, and every wait ends. Where the machine can't
//! hold the most shares a test takes beside the reserve, every server keeps its store on disk,
//! and the heavy turn still goes one at a time, which bounds the disk the suite takes.

use std::sync::{Condvar, Mutex, OnceLock, PoisonError};

use tokio::sync::Semaphore;

use crate::docker::{compose, docker};

/// What a server may hold in memory, its store and its processes together, in mebibytes, its
/// limit in its share: a
/// bundled database's server that restores twice holds up to 2.2 gibibytes that its kernel can't
/// reclaim, its store's files and its processes' memory, beside a cache that it can. The limit is
/// Docker's, so a process on a server that would take more ends, and its test fails, while the
/// machine keeps the rest.
pub const SERVER_MIB: u64 = 3_072;
/// What a managed database's server may hold in memory, its limit in its deployment's share: its
/// data and its processes, up to 160 mebibytes.
pub const DATABASE_MIB: u64 = 512;
/// What the server that holds the heavy turn may hold beyond its own limit: a bundled database's
/// server with the self-hosted stack holds up to 6.1 gibibytes.
pub const HEAVY_MIB: u64 = 4_096;
/// What the machine keeps beside the servers, for the tests' own processes.
const HEADROOM_MIB: u64 = 1_024;
/// The most shares that a test takes, for its two servers (`Server::start_two`).
const MOST_SHARES_A_TEST_TAKES: u64 = 2;

/// The shares of the servers' memory, or none where the servers keep their stores on disk.
pub fn shares() -> Option<&'static Turns> {
    static SHARES: OnceLock<Option<Turns>> = OnceLock::new();
    SHARES
        .get_or_init(|| {
            let shares = shares_in(available_mib());
            (shares > 0).then(|| Turns::new(shares))
        })
        .as_ref()
}

/// How many shares `available` mebibytes hold beside the heavy turn's reserve and the headroom,
/// or none when they hold fewer than a test takes.
fn shares_in(available: u64) -> u64 {
    let shares = available.saturating_sub(HEAVY_MIB + HEADROOM_MIB) / (SERVER_MIB + DATABASE_MIB);
    if shares < MOST_SHARES_A_TEST_TAKES {
        0
    } else {
        shares
    }
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
    /// How many turns there are.
    count: u64,
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
            count,
            queue: Mutex::new(Queue {
                free: count,
                next: 0,
                serving: 0,
            }),
            changed: Condvar::new(),
        }
    }

    /// `count` turns, once they are free and every thread that asked earlier has its own. More
    /// turns than there are fail at once, since they would never be free.
    pub fn take(&'static self, count: u64) -> Turn {
        assert!(
            count <= self.count,
            "a test takes {count} turns of {}, which it would wait for for ever",
            self.count
        );
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

mod tests {
    use super::{DATABASE_MIB, HEADROOM_MIB, HEAVY_MIB, SERVER_MIB, Turns, shares_in};

    #[test]
    fn test_06_a_machine_without_room_for_a_test_s_two_servers_keeps_the_stores_on_disk() {
        let reserved = HEAVY_MIB + HEADROOM_MIB;
        let share = SERVER_MIB + DATABASE_MIB;
        assert_eq!(shares_in(reserved + share), 0);
        assert_eq!(shares_in(reserved + 2 * share - 1), 0);
        assert_eq!(shares_in(reserved + 2 * share), 2);
        assert_eq!(shares_in(reserved + 3 * share), 3);
    }

    #[test]
    #[should_panic(expected = "a test takes 2 turns of 1, which it would wait for for ever")]
    fn test_06_more_turns_than_there_are_fail_at_once() {
        let turns: &'static Turns = Box::leak(Box::new(Turns::new(1)));
        let _turn = turns.take(2);
    }
}
