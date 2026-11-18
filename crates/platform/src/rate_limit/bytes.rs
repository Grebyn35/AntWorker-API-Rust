//! Budgets: they count the bytes of a kind of request in windows, where a bucket counts
//! requests, such as the batches of telemetry that a user's clients send. A subject's budget
//! counts in the database, in the buckets' table, and a replica's budget, of everything that the
//! replica serves, in the replica's memory (PLATFORM-089). A budget's name, limit and window are
//! held as a [`Bucket`] holds a bucket's, with the limit in bytes.

use std::{
    sync::{Arc, Mutex, PoisonError},
    time::Instant,
};

use super::{Bucket, Count, LimitError, RateLimiter, RateLimits};
use crate::{clock::SharedClock, errors::Internal};

impl RateLimiter {
    /// Counts `bytes` in the budget of `budget`'s kind for `value`, such as a user's ID, in one
    /// statement that commits at once, as [`RateLimiter::count`] counts a request (PLATFORM-055).
    async fn spend(&self, budget: Bucket, value: &[u8], bytes: u32) -> Result<Count, Internal> {
        let key = self.key.hash(budget.name, value);
        let (count, resets_in) = self.database.spend(&key, budget.window, bytes).await?;
        Ok(counted(budget, count, resets_in))
    }
}

/// The state of `budget` once its window has counted `count` bytes and ends in `resets_in`.
fn counted(budget: Bucket, count: u32, resets_in: std::time::Duration) -> Count {
    Count {
        bucket: budget,
        count,
        resets_in,
        bytes: true,
    }
}

impl RateLimits {
    /// Counts `bytes` in the budget of `budget`'s kind for `value`, such as a user's ID, and
    /// refuses the request once they take the window's count beyond the budget's limit. The
    /// bytes of a refused request stay counted (PLATFORM-089).
    pub async fn spend(&self, budget: Bucket, value: &[u8], bytes: u32) -> Result<(), LimitError> {
        let count = self.limiter.spend(budget, value, bytes).await?;
        self.settle(count)
    }

    /// Counts `bytes` in `budget`, the replica's, and refuses the request once they take the
    /// window's count beyond the budget's limit. The bytes of a refused request stay counted
    /// (PLATFORM-089).
    pub fn spend_on_replica(&self, budget: &ReplicaBudget, bytes: u32) -> Result<(), LimitError> {
        self.settle(budget.count(bytes))
    }

    /// Refuses the request when `count` is beyond its budget. A budget joins the request's counts
    /// only then, so the headers of a response that no budget refused describe a bucket of
    /// requests (PLATFORM-056).
    fn settle(&self, count: Count) -> Result<(), LimitError> {
        if count.refused() {
            self.counted.push(count);
            return Err(LimitError::RateLimited);
        }
        Ok(())
    }
}

/// A budget that a replica counts in its own memory, in no bucket of the database's, for
/// everything it serves, so the deployment's budget is this one for each replica it runs
/// (PLATFORM-089). A window starts with the first bytes counted after the last window ended, by
/// the replica's clock.
#[derive(Clone)]
pub struct ReplicaBudget {
    budget: Bucket,
    clock: SharedClock,
    window: Arc<Mutex<Window>>,
}

/// A window of a replica's budget: when it started, and the bytes it has counted.
#[derive(Debug, Default)]
struct Window {
    started: Option<Instant>,
    spent: u32,
}

impl ReplicaBudget {
    /// The replica's budget of `budget`'s name, limit and window, on `clock`.
    pub fn new(budget: Bucket, clock: SharedClock) -> Self {
        Self {
            budget,
            clock,
            window: Arc::default(),
        }
    }

    /// Counts `bytes` in the window, which starts again when the last one has ended.
    fn count(&self, bytes: u32) -> Count {
        let now = self.clock.now();
        // A lock that a panic poisoned still holds a whole window, since nothing between its two
        // writes can panic, so its data is taken as it is.
        let mut window = self.window.lock().unwrap_or_else(PoisonError::into_inner);
        let started = match window.started {
            Some(started) if now.duration_since(started) < self.budget.window => started,
            _ => {
                window.started = Some(now);
                window.spent = 0;
                now
            }
        };
        window.spent = window.spent.saturating_add(bytes);
        let resets_in = self
            .budget
            .window
            .saturating_sub(now.duration_since(started));
        counted(self.budget, window.spent, resets_in)
    }
}

impl std::fmt::Debug for ReplicaBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplicaBudget")
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::clock::{Clock, Sleep};

    /// A clock that a test moves.
    struct Moved {
        start: Instant,
        elapsed: Mutex<Duration>,
    }

    impl Moved {
        #[expect(
            clippy::disallowed_methods,
            reason = "the test's clock starts at the real time once, then moves only when advanced"
        )]
        fn new() -> Arc<Self> {
            Arc::new(Self {
                start: Instant::now(),
                elapsed: Mutex::default(),
            })
        }

        fn advance(&self, duration: Duration) {
            *self.elapsed.lock().unwrap() += duration;
        }
    }

    impl Clock for Moved {
        fn now(&self) -> Instant {
            self.start + *self.elapsed.lock().unwrap()
        }

        fn sleep(&self, _: Duration) -> Sleep {
            Box::pin(std::future::pending())
        }
    }

    const BUDGET: Bucket = Bucket {
        name: "replica",
        limit: 100,
        window: Duration::from_secs(60),
    };

    #[test]
    fn platform_089_a_replica_s_budget_refuses_the_bytes_that_take_its_window_beyond_its_limit() {
        let budget = ReplicaBudget::new(BUDGET, Moved::new());

        let first = budget.count(60);
        let full = budget.count(40);
        let beyond = budget.count(1);

        assert!(!first.refused());
        assert_eq!(first.remaining(), 40);
        assert!(!full.refused(), "the budget's last byte is within it");
        assert_eq!(full.remaining(), 0);
        assert!(beyond.refused());
        assert!(beyond.bytes);
    }

    #[test]
    fn platform_089_a_refused_request_s_bytes_stay_counted_until_the_window_ends() {
        let clock = Moved::new();
        let budget = ReplicaBudget::new(BUDGET, clock.clone());
        assert!(!budget.count(90).refused());
        assert!(budget.count(20).refused());

        clock.advance(Duration::from_secs(59));
        let within = budget.count(1);
        clock.advance(Duration::from_secs(1));
        let after = budget.count(100);

        assert!(within.refused(), "the refused bytes left no room for one");
        assert_eq!(within.resets_in, Duration::from_secs(1));
        assert!(!after.refused(), "a window that ended starts again");
        assert_eq!(after.resets_in, Duration::from_secs(60));
    }
}
