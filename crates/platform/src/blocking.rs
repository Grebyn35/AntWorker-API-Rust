//! CPU-heavy work, such as hashing a password: it runs on a blocking thread, so it never stalls
//! the async runtime (CODE-05), inside a span of its own, so it shows in traces (OBS-06), and at
//! most as many at once as the instance has processors, so a burst of it takes no more memory
//! than that many works do (OPS-07, PLATFORM-060). A caller that must read what it works on
//! first, inside a unit of work, takes its [`Turn`] before it opens the unit of work, so it waits
//! for the turn while it holds no connection of the pool.

#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{error::Error, num::NonZeroUsize, sync::Arc, thread};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::Instrument;

use crate::telemetry::names;

/// The turns of an instance's CPU-heavy work, one per processor it may use. `main.rs` makes one
/// per process, and every caller shares it through its clones.
#[derive(Clone)]
pub struct Workers {
    turns: Arc<Semaphore>,
    count: usize,
    /// How many callers wait for a turn, which the tests read.
    #[cfg(feature = "test-support")]
    waiting: Arc<AtomicUsize>,
}

impl Default for Workers {
    fn default() -> Self {
        Self::new()
    }
}

impl Workers {
    /// One turn per processor the instance may use: its CPU limit, such as a container's, rounded
    /// down, or every processor it may run on without one, and at least one.
    pub fn new() -> Self {
        let count = thread::available_parallelism().map_or(1, NonZeroUsize::get);
        Self {
            turns: Arc::new(Semaphore::new(count)),
            count,
            #[cfg(feature = "test-support")]
            waiting: Arc::default(),
        }
    }

    /// How many works run at once, at most.
    pub fn turns(&self) -> usize {
        self.count
    }

    /// How many callers wait for a turn now. A test reads it to wait for a state, and asserts
    /// only which works start, which a caller observes (TEST-06, TEST-07).
    #[cfg(feature = "test-support")]
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }

    /// How many turns no work holds now, which a test reads as it reads [`Self::waiting`].
    #[cfg(feature = "test-support")]
    pub fn free(&self) -> usize {
        self.turns.available_permits()
    }

    /// Runs `work` on a blocking thread once a turn is free, inside a span named `name`, a child
    /// of the current span, which covers the wait for the turn too. A caller that stops waiting,
    /// such as a request past its timeout, gives up its place, and its work never runs. A work
    /// that started keeps its turn until it ends, even when its caller stopped waiting. Fails
    /// only when `work` panics.
    pub async fn run<T>(
        &self,
        name: &'static str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Failed>
    where
        T: Send + 'static,
    {
        let span = tracing::info_span!("blocking", { names::OTEL_NAME } = name);
        let turn = self.wait(&span).await?;
        let (ran, _) = turn.run_in(span, work).await?;
        Ok(ran)
    }

    /// Waits for a turn, inside a span of its own, named `turn`, and gives it to its caller, who
    /// runs its works in it, one after another, through [`Turn::run`]. The turn goes once its
    /// holder has dropped it and the work it started, if any, has ended. A caller takes it this
    /// way before it opens a unit of work whose reads the work needs, such as a sign-in's of the
    /// password's hash, so that it waits for the turn with no pooled connection, and holds one
    /// only while it works. A caller that stops waiting gives up its place. Its error stands for
    /// a closed semaphore, which `Workers` never closes, so it fails in no way a caller meets.
    pub async fn turn(&self) -> Result<Turn, Failed> {
        let span = tracing::info_span!("blocking", { names::OTEL_NAME } = "turn");
        self.wait(&span).await
    }

    /// Waits for a turn, inside `span`.
    async fn wait(&self, span: &tracing::Span) -> Result<Turn, Failed> {
        #[cfg(feature = "test-support")]
        let waiting = Waiting::enter(&self.waiting);
        let permit = self
            .turns
            .clone()
            .acquire_owned()
            .instrument(span.clone())
            .await
            .map_err(|closed| Failed(closed.into()))?;
        #[cfg(feature = "test-support")]
        drop(waiting);
        Ok(Turn(Arc::new(permit)))
    }
}

/// A turn at the instance's CPU-heavy work, which [`Workers::turn`] gives. Its holder runs works
/// in it, one after another, and it goes once the holder has dropped it and the work it started
/// has ended, so a work keeps its turn until it ends even when its caller stopped waiting
/// (PLATFORM-060).
pub struct Turn(Arc<OwnedSemaphorePermit>);

impl Turn {
    /// Runs `work` on a blocking thread, in this turn, inside a span named `name`, a child of the
    /// current span, and gives the turn back with what the work returned, for the holder's next
    /// work. It takes the turn while the work runs, so the holder's works run one after another,
    /// and a holder that stops waiting for a work has given the turn up with it, so it starts no
    /// other beside it. Fails only when `work` panics, and the turn goes with the failure.
    pub async fn run<T>(
        self,
        name: &'static str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<(T, Self), Failed>
    where
        T: Send + 'static,
    {
        let span = tracing::info_span!("blocking", { names::OTEL_NAME } = name);
        self.run_in(span, work).await
    }

    /// Runs `work` on a blocking thread, in this turn, inside `span`, and gives the turn back
    /// with what the work returned.
    async fn run_in<T>(
        self,
        span: tracing::Span,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<(T, Self), Failed>
    where
        T: Send + 'static,
    {
        let turn = self.0.clone();
        #[expect(
            clippy::disallowed_methods,
            reason = "Workers is the one place that moves work to a blocking thread (CODE-05)"
        )]
        let ran = tokio::task::spawn_blocking(move || {
            let _turn = turn;
            span.in_scope(work)
        });
        let ran = ran.await.map_err(|panicked| Failed(panicked.into()))?;
        Ok((ran, self))
    }
}

/// A caller waiting for a turn, counted until it takes one or stops waiting.
#[cfg(feature = "test-support")]
struct Waiting(Arc<AtomicUsize>);

#[cfg(feature = "test-support")]
impl Waiting {
    fn enter(waiting: &Arc<AtomicUsize>) -> Self {
        waiting.fetch_add(1, Ordering::SeqCst);
        Self(waiting.clone())
    }
}

#[cfg(feature = "test-support")]
impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// CPU-heavy work that ended without a result: it panicked. Its source also covers a closed
/// semaphore, which `Workers` never closes.
#[derive(Debug, thiserror::Error)]
#[error("the CPU-heavy work ended without a result")]
pub struct Failed(#[source] Box<dyn Error + Send + Sync>);
