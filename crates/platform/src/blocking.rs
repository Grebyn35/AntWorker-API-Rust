//! CPU-heavy work, such as hashing a password: it runs on a blocking thread, so it never stalls
//! the async runtime (CODE-05), inside a span of its own, so it shows in traces (OBS-06), and at
//! most as many at once as the instance has processors, so a burst of it takes no more memory
//! than that many works do (OPS-07, PLATFORM-078).

use std::{error::Error, num::NonZeroUsize, sync::Arc, thread};

use tokio::sync::Semaphore;
use tracing::Instrument;

use crate::telemetry::names;

/// The turns of an instance's CPU-heavy work, one per processor it may use. `main.rs` makes one
/// per process, and every caller shares it through its clones.
#[derive(Clone)]
pub struct Workers {
    turns: Arc<Semaphore>,
    count: usize,
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
        }
    }

    /// How many works run at once, at most.
    pub fn turns(&self) -> usize {
        self.count
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
        let turn = self
            .turns
            .clone()
            .acquire_owned()
            .instrument(span.clone())
            .await
            .map_err(|closed| Failed(closed.into()))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "Workers is the one place that moves work to a blocking thread (CODE-05)"
        )]
        let ran = tokio::task::spawn_blocking(move || {
            let _turn = turn;
            span.in_scope(work)
        });
        ran.await.map_err(|panicked| Failed(panicked.into()))
    }
}

/// CPU-heavy work that ended without a result: it panicked. Its source also covers a closed
/// semaphore, which `Workers` never closes.
#[derive(Debug, thiserror::Error)]
#[error("the CPU-heavy work ended without a result")]
pub struct Failed(#[source] Box<dyn Error + Send + Sync>);
