//! Time for code that measures or waits (CODE-06). Every read of the time, and every wait for a
//! time to pass, in the workspace goes through a [`Clock`], so the tests control time (TEST-06),
//! but for the waits on the real time and the reads of the system's time that CODE-06 names.
#![expect(
    clippy::disallowed_types,
    reason = "the system clock is defined here, and only the binary's main.rs uses it (CODE-06)"
)]

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

/// A source of time, and of waits measured by it. The system clock serves the API, and the
/// test clock, which only the tests compile, `platform`'s own and, with the `test-support`
/// feature, those of other crates, serves them (ARCH-13).
pub trait Clock: Send + Sync + 'static {
    /// The current instant.
    fn now(&self) -> Instant;

    /// Completes once `duration` has passed on this clock.
    fn sleep(&self, duration: Duration) -> Sleep;
}

/// A wait on a [`Clock`].
pub type Sleep = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The clock that every part of the API shares.
pub type SharedClock = Arc<dyn Clock>;

/// The operating system's monotonic clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the system clock reads the time for the code that CODE-06 holds to a clock"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the system clock waits for the code that CODE-06 holds to a clock"
    )]
    fn sleep(&self, duration: Duration) -> Sleep {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[cfg(any(test, feature = "test-support"))]
pub use testing::TestClock;

#[cfg(any(test, feature = "test-support"))]
mod testing {
    use std::{
        sync::{Arc, Mutex, Weak},
        time::{Duration, Instant},
    };

    use tokio::sync::Notify;

    use super::{Clock, Sleep};
    use crate::errors::Held;

    /// A clock that moves only when a test advances it. A sleep on it completes once the test
    /// advances the clock to the sleep's deadline.
    #[derive(Debug)]
    pub struct TestClock {
        start: Instant,
        /// The time that has passed, and the sleeps that wait. A lock that a panic poisoned still
        /// holds a whole state, since no change of it can panic once it has written a part of it,
        /// so it is taken as it is.
        state: Mutex<State>,
        slept: Notify,
    }

    #[derive(Debug, Default)]
    struct State {
        elapsed: Duration,
        sleepers: Vec<Sleeper>,
    }

    /// A sleep waiting on the clock. The sleep holds its `Notify`, so a sleep that was dropped
    /// leaves only a dead reference here.
    #[derive(Debug)]
    struct Sleeper {
        deadline: Duration,
        duration: Duration,
        wake: Weak<Notify>,
    }

    impl Default for TestClock {
        #[expect(
            clippy::disallowed_methods,
            reason = "the test clock starts at the real time once, then moves only when advanced"
        )]
        fn default() -> Self {
            Self {
                start: Instant::now(),
                state: Mutex::default(),
                slept: Notify::new(),
            }
        }
    }

    impl TestClock {
        /// Moves the clock forward by `duration`, and completes every sleep whose deadline it
        /// reaches.
        pub fn advance(&self, duration: Duration) {
            let mut state = self.state.lock().held();
            state.elapsed += duration;
            let now = state.elapsed;
            let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut state.sleepers)
                .into_iter()
                .partition(|sleeper| sleeper.deadline <= now);
            state.sleepers = waiting;
            for wake in due.iter().filter_map(|sleeper| sleeper.wake.upgrade()) {
                wake.notify_one();
            }
        }

        /// Completes once a sleep of `duration` is waiting on this clock, so a test advances
        /// the clock only after the code under test has started that wait. Another task's sleep
        /// of the same duration completes it too, so a test of a wait that may share its
        /// duration with another first waits for what the code does just before it sleeps, such
        /// as its request reaching a stand-in. A sleep that the code starts after work of its
        /// own, as a poll follows a query, may start after the test moved the clock on, and no
        /// earlier advance ends it: such a test advances each time the sleep waits, until what
        /// it waits for has happened.
        pub async fn sleeping(&self, duration: Duration) {
            loop {
                let slept = self.slept.notified();
                let mut slept = std::pin::pin!(slept);
                // Registers before the check, so a sleep that starts in between still wakes it.
                slept.as_mut().enable();
                if self.waiting(duration) {
                    return;
                }
                slept.await;
            }
        }

        fn waiting(&self, duration: Duration) -> bool {
            let state = self.state.lock().held();
            state
                .sleepers
                .iter()
                .any(|sleeper| sleeper.duration == duration && sleeper.wake.strong_count() > 0)
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            let state = self.state.lock().held();
            self.start + state.elapsed
        }

        fn sleep(&self, duration: Duration) -> Sleep {
            let wake = Arc::new(Notify::new());
            {
                let mut state = self.state.lock().held();
                let deadline = state.elapsed + duration;
                if deadline <= state.elapsed {
                    return Box::pin(async {});
                }
                state.sleepers.push(Sleeper {
                    deadline,
                    duration,
                    wake: Arc::downgrade(&wake),
                });
            }
            self.slept.notify_waiters();
            // A wake that comes before the sleep is polled is kept until it is.
            Box::pin(async move { wake.notified().await })
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::Arc;

        use super::*;

        #[tokio::test]
        async fn test_06_a_sleep_completes_when_the_clock_reaches_its_deadline() {
            let clock = Arc::new(TestClock::default());
            let start = clock.now();
            let sleeper = tokio::spawn(clock.sleep(Duration::from_secs(5)));
            clock.sleeping(Duration::from_secs(5)).await;

            clock.advance(Duration::from_secs(4));
            tokio::task::yield_now().await;
            assert!(!sleeper.is_finished());

            clock.advance(Duration::from_secs(1));
            sleeper.await.unwrap();
            assert_eq!(clock.now() - start, Duration::from_secs(5));
        }

        #[tokio::test]
        async fn test_06_a_sleep_of_nothing_completes_at_once() {
            let clock = TestClock::default();
            clock.sleep(Duration::ZERO).await;
        }

        #[test]
        fn test_06_a_sleep_that_was_dropped_waits_no_more() {
            let clock = TestClock::default();
            let duration = Duration::from_secs(5);
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            drop(clock.sleep(duration));

            let mut sleeping = std::pin::pin!(clock.sleeping(duration));
            let before = sleeping.as_mut().poll(&mut context).is_ready();
            let _waiting = clock.sleep(duration);
            // A wait that completed is polled no more.
            let after = (!before).then(|| sleeping.as_mut().poll(&mut context).is_ready());

            assert!(!before, "a sleep that was dropped still waits");
            assert_eq!(after, Some(true), "a sleep that waits isn't seen");
        }
    }
}
