//! A subscriber of a test's own, which knows which of the spans that opened under it are still
//! open, for the tests of when a span ends: `tracing` closes a span once the last handle to it
//! drops, such as a unit of work's (PLATFORM-104).

use std::sync::{Arc, Mutex, PoisonError};

use tracing::{
    Dispatch, Subscriber,
    span::{Attributes, Id},
};
use tracing_subscriber::{
    Layer,
    layer::{Context, SubscriberExt},
    registry::LookupSpan,
};

/// The spans that opened under [`OpenSpans::dispatch`] and haven't closed, by their names. A
/// lock that a panic poisoned still holds whole spans, since each change of them is one call, so
/// it is taken as it is.
#[derive(Clone, Default)]
pub struct OpenSpans(Arc<Mutex<Vec<(Id, &'static str)>>>);

impl OpenSpans {
    /// A subscriber that records its spans here, which a test makes the default while it runs
    /// its work, as `tracing::instrument::WithSubscriber` does.
    pub fn dispatch(&self) -> Dispatch {
        Dispatch::new(tracing_subscriber::registry().with(self.clone()))
    }

    /// Whether a span named `name` is open.
    pub fn is_open(&self, name: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(_, open)| *open == name)
    }
}

impl<S> Layer<S> for OpenSpans
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, span: &Attributes<'_>, id: &Id, _: Context<'_, S>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((id.clone(), span.metadata().name()));
    }

    fn on_close(&self, id: Id, _: Context<'_, S>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|(open, _)| *open != id);
    }
}
