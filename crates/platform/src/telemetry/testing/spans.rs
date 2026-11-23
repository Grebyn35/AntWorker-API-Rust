//! The subscribers of a test's own: one that knows which of the spans that opened under it are
//! still open, for the tests of when a span ends, since `tracing` closes a span once the last
//! handle to it drops, such as a unit of work's (PLATFORM-104), and one that keeps the spans that
//! ended under it, as the API exports them, for the tests of what a span ends with, such as its
//! status (PLATFORM-020).

use std::sync::{Arc, Mutex};

use opentelemetry::trace::Status;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tracing::{
    Dispatch, Subscriber,
    span::{Attributes, Id},
};
use tracing_subscriber::{
    Layer,
    layer::{Context, SubscriberExt},
    registry::LookupSpan,
};

use crate::errors::Held;

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
        self.0.lock().held().iter().any(|(_, open)| *open == name)
    }
}

impl<S> Layer<S> for OpenSpans
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, span: &Attributes<'_>, id: &Id, _: Context<'_, S>) {
        self.0
            .lock()
            .held()
            .push((id.clone(), span.metadata().name()));
    }

    fn on_close(&self, id: Id, _: Context<'_, S>) {
        self.0.lock().held().retain(|(open, _)| *open != id);
    }
}

/// The spans that ended under [`EndedSpans::dispatch`], as the API's layer of spans makes them,
/// which an in-memory exporter keeps.
#[derive(Clone)]
pub struct EndedSpans {
    tracer: SdkTracerProvider,
    exporter: InMemorySpanExporter,
}

impl Default for EndedSpans {
    fn default() -> Self {
        let exporter = InMemorySpanExporter::default();
        let tracer = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        Self { tracer, exporter }
    }
}

impl EndedSpans {
    /// A subscriber that records its spans here once they end, which a test makes the default
    /// while it runs its work, as `tracing::dispatcher::set_default` does for every task that
    /// runs on the test's thread.
    pub fn dispatch(&self) -> Dispatch {
        Dispatch::new(tracing_subscriber::registry().with(crate::telemetry::spans(&self.tracer)))
    }

    /// The status of each span named `name` that has ended, in the order they ended: `error`,
    /// `ok` or `unset`.
    pub fn statuses(&self, name: &str) -> Vec<&'static str> {
        self.ended(name)
            .iter()
            .map(|span| match span.status {
                Status::Error { .. } => "error",
                Status::Ok => "ok",
                Status::Unset => "unset",
            })
            .collect()
    }

    /// The attribute `key` of each span named `name` that has ended, in the order they ended, as
    /// text, or `None` for a span without it.
    pub fn attributes(&self, name: &str, key: &str) -> Vec<Option<String>> {
        self.ended(name)
            .iter()
            .map(|span| {
                span.attributes
                    .iter()
                    .find(|attribute| attribute.key.as_str() == key)
                    .map(|attribute| attribute.value.to_string())
            })
            .collect()
    }

    /// Each span named `name` that has ended, in the order they ended.
    fn ended(&self, name: &str) -> Vec<SpanData> {
        let ended = self.exporter.get_finished_spans().unwrap_or_else(|error| {
            crate::errors::ignore(&error, "test_spans_not_read");
            Vec::new()
        });
        ended.into_iter().filter(|span| span.name == name).collect()
    }
}
