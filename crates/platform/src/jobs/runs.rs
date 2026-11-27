//! The runs that a dispatcher or a scheduler has in flight, each with its claim, which a shutdown
//! waits for until its deadline, and then cancels (PLATFORM-018).

use std::{collections::HashMap, future::Future};

use tokio::{
    sync::watch,
    task::{self, JoinSet},
};

/// The runs in flight, with the claim each holds.
pub(super) struct Runs<C> {
    set: JoinSet<()>,
    claims: HashMap<task::Id, C>,
}

impl<C> Runs<C> {
    /// No runs.
    pub(super) fn new() -> Self {
        Self {
            set: JoinSet::new(),
            claims: HashMap::new(),
        }
    }

    /// How many runs are in flight.
    pub(super) fn len(&self) -> usize {
        self.set.len()
    }

    /// Whether no run is in flight.
    pub(super) fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Starts `run`, which holds `claim`.
    pub(super) fn spawn(&mut self, claim: C, run: impl Future<Output = ()> + Send + 'static) {
        let handle = self.set.spawn(run);
        self.claims.insert(handle.id(), claim);
    }

    /// Waits for a run to end. A run that panicked ended too: the panic hook logged it, and its
    /// claim's lease runs out, so another replica runs the job again.
    pub(super) async fn next(&mut self) {
        if let Some(joined) = self.set.join_next_with_id().await {
            let id = match joined {
                Ok((id, ())) => id,
                Err(error) => error.id(),
            };
            self.claims.remove(&id);
        }
    }

    /// Waits for every run until `deadline` passes, then cancels those still going, and returns
    /// their claims, which the caller releases.
    pub(super) async fn drain(mut self, mut deadline: watch::Receiver<bool>) -> Vec<C> {
        let passed = deadline.wait_for(|passed| *passed);
        tokio::pin!(passed);
        loop {
            tokio::select! {
                joined = self.set.join_next_with_id() => match joined {
                    None => return Vec::new(),
                    Some(Ok((id, ()))) => {
                        self.claims.remove(&id);
                    }
                    Some(Err(error)) => {
                        self.claims.remove(&error.id());
                    }
                },
                // The deadline's sender drops only once it has passed.
                _ = &mut passed => break,
            }
        }
        self.set.abort_all();
        while let Some(joined) = self.set.join_next_with_id().await {
            match joined {
                // It ended before the cancellation reached it, so it holds no claim.
                Ok((id, ())) => {
                    self.claims.remove(&id);
                }
                Err(error) if !error.is_cancelled() => {
                    self.claims.remove(&error.id());
                }
                Err(_) => {}
            }
        }
        self.claims.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;
    use tokio::sync::{oneshot, watch};

    use super::Runs;

    #[tokio::test]
    async fn platform_016_the_runs_in_flight_count_until_each_ends() {
        let mut runs = Runs::new();
        let idle = runs.is_empty();
        let (end, ended) = oneshot::channel::<()>();
        runs.spawn("claim", async move { ended.await.unwrap() });
        let (running, in_flight) = (runs.len(), runs.is_empty());

        // The wait for a run to end goes on while the run does.
        let waited = runs.next().now_or_never().is_none();
        end.send(()).unwrap();
        runs.next().await;

        assert!(idle);
        assert_eq!((running, in_flight), (1, false));
        assert!(waited, "the wait ended while the run went on");
        assert_eq!((runs.len(), runs.is_empty()), (0, true));
    }

    /// A value whose drop panics, as a run's cancellation may meet a panic.
    struct PanicsWhenDropped;

    impl Drop for PanicsWhenDropped {
        #[expect(
            clippy::panic,
            reason = "the test's run panics as it is cancelled, as a run with a bug may"
        )]
        fn drop(&mut self) {
            panic!("the run panicked as it was cancelled");
        }
    }

    #[tokio::test]
    async fn platform_018_a_run_that_panics_as_it_is_cancelled_is_left_to_its_lease() {
        let mut runs = Runs::new();
        runs.spawn("cancelled", std::future::pending());
        let panics = PanicsWhenDropped;
        runs.spawn("panics", async move {
            let _panics = panics;
            std::future::pending::<()>().await;
        });
        let (_passed, deadline) = watch::channel(true);

        let released = runs.drain(deadline).await;

        assert_eq!(released, ["cancelled"]);
    }
}
