//! Time for code that measures or waits (CODE-06). Every read of the time and every wait in the
//! workspace goes through a [`Clock`], so the tests control time (TEST-07).

use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

/// A source of time, and of waits measured by it. The system clock serves the API, and the
/// test clock, which only the `test-support` feature compiles, serves the tests (ARCH-13).
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
        reason = "the system clock is the one place that reads the time (CODE-06)"
    )]
    fn now(&self) -> Instant {
        Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the system clock is the one place that waits (CODE-06)"
    )]
    fn sleep(&self, duration: Duration) -> Sleep {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[cfg(feature = "test-support")]
pub use testing::TestClock;

#[cfg(feature = "test-support")]
mod testing {
    use std::{
        sync::{Arc, Mutex, PoisonError, Weak},
        time::{Duration, Instant},
    };

    use tokio::sync::Notify;

    use super::{Clock, Sleep};

    /// A clock that moves only when a test advances it. A sleep on it completes once the test
    /// advances the clock to the sleep's deadline.
    #[derive(Debug)]
    pub struct TestClock {
        start: Instant,
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
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
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
        /// the clock only after the code under test has started that wait.
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
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state
                .sleepers
                .iter()
                .any(|sleeper| sleeper.duration == duration && sleeper.wake.strong_count() > 0)
        }
    }

    impl Clock for TestClock {
        fn now(&self) -> Instant {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            self.start + state.elapsed
        }

        fn sleep(&self, duration: Duration) -> Sleep {
            let wake = Arc::new(Notify::new());
            {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
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
        async fn test_07_a_sleep_completes_when_the_clock_reaches_its_deadline() {
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
        async fn test_07_a_sleep_of_nothing_completes_at_once() {
            let clock = TestClock::default();
            clock.sleep(Duration::ZERO).await;
        }
    }
}
