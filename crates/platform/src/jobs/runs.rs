//! The runs that a dispatcher or a scheduler has in flight, each with its claim, which a shutdown
//! waits for until its deadline, and then cancels (PLATFORM-063).

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
