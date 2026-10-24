//! A stand-in for the OpenTelemetry Collector (TEST-05): an OTLP/HTTP receiver on a free port of
//! 127.0.0.1 that keeps every request it receives, decoded. It runs on the async runtime that
//! starts it, and stops when it is dropped.

use std::{
    io,
    net::SocketAddr,
    sync::{Arc, Mutex, PoisonError},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use opentelemetry_proto::tonic::{
    collector::{
        logs::v1::ExportLogsServiceRequest, metrics::v1::ExportMetricsServiceRequest,
        trace::v1::ExportTraceServiceRequest,
    },
    common::v1::{AnyValue, KeyValue, any_value},
    logs::v1::LogRecord,
    metrics::v1::{Metric, metric, number_data_point},
    resource::v1::Resource,
    trace::v1::Span,
};
use prost::Message;
use tokio::{net::TcpListener, sync::Notify};

/// Everything the collector received, in order.
#[derive(Clone, Debug, Default)]
pub struct Received {
    /// Each export of spans.
    pub traces: Vec<ExportTraceServiceRequest>,
    /// Each export of log records.
    pub logs: Vec<ExportLogsServiceRequest>,
    /// Each export of metrics.
    pub metrics: Vec<ExportMetricsServiceRequest>,
}

impl Received {
    /// Every span received, with the resource it came from.
    pub fn spans(&self) -> Vec<(&Resource, &Span)> {
        self.traces
            .iter()
            .flat_map(|export| &export.resource_spans)
            .flat_map(|resource| {
                resource
                    .scope_spans
                    .iter()
                    .flat_map(|scope| &scope.spans)
                    .filter_map(move |span| Some((resource.resource.as_ref()?, span)))
            })
            .collect()
    }

    /// Every log record received, with the resource it came from.
    pub fn records(&self) -> Vec<(&Resource, &LogRecord)> {
        self.logs
            .iter()
            .flat_map(|export| &export.resource_logs)
            .flat_map(|resource| {
                resource
                    .scope_logs
                    .iter()
                    .flat_map(|scope| &scope.log_records)
                    .filter_map(move |record| Some((resource.resource.as_ref()?, record)))
            })
            .collect()
    }

    /// Every metric received, with the resource it came from, from the last export first.
    pub fn metrics(&self) -> Vec<(&Resource, &Metric)> {
        self.metrics
            .iter()
            .rev()
            .flat_map(|export| &export.resource_metrics)
            .flat_map(|resource| {
                resource
                    .scope_metrics
                    .iter()
                    .flat_map(|scope| &scope.metrics)
                    .filter_map(move |metric| Some((resource.resource.as_ref()?, metric)))
            })
            .collect()
    }

    /// The value of the counter `name` in the last export that holds it, for the data point
    /// whose `error.type` is `error_type`, or that has none. Zero if no export holds it.
    pub fn count(&self, name: &str, error_type: Option<&str>) -> i64 {
        self.metrics()
            .into_iter()
            .find(|(_, metric)| metric.name == name)
            .and_then(|(_, metric)| match &metric.data {
                Some(metric::Data::Sum(sum)) => sum
                    .data_points
                    .iter()
                    .find(|point| {
                        attribute(&point.attributes, "error.type").as_deref() == error_type
                    })
                    .and_then(|point| match point.value {
                        Some(number_data_point::Value::AsInt(value)) => Some(value),
                        _ => None,
                    }),
                _ => None,
            })
            .unwrap_or(0)
    }
}

/// The text of the attribute `key` among `attributes`, if it has one.
pub fn attribute(attributes: &[KeyValue], key: &str) -> Option<String> {
    attributes
        .iter()
        .find(|pair| pair.key == key)
        .and_then(|pair| pair.value.as_ref())
        .and_then(text)
}

/// A value as text: a string as itself, a number in decimal.
pub fn text(value: &AnyValue) -> Option<String> {
    match value.value.as_ref()? {
        any_value::Value::StringValue(text) => Some(text.clone()),
        any_value::Value::IntValue(number) => Some(number.to_string()),
        any_value::Value::BoolValue(boolean) => Some(boolean.to_string()),
        _ => None,
    }
}

/// The running stand-in. Dropping it stops it.
pub struct Collector {
    address: SocketAddr,
    state: Arc<Shared>,
    server: tokio::task::JoinHandle<()>,
}

struct Shared {
    received: Mutex<Received>,
    changed: Notify,
    refuse_logs: bool,
}

impl Collector {
    /// A collector that accepts every export. Fails if it can't listen.
    pub async fn start() -> io::Result<Self> {
        Self::listening(false).await
    }

    /// A collector that answers every export of log records with 400, which the exporter
    /// counts as a failed export and doesn't retry. Fails if it can't listen.
    pub async fn refusing_logs() -> io::Result<Self> {
        Self::listening(true).await
    }

    async fn listening(refuse_logs: bool) -> io::Result<Self> {
        let state = Arc::new(Shared {
            received: Mutex::default(),
            changed: Notify::new(),
            refuse_logs,
        });
        #[expect(
            clippy::disallowed_methods,
            reason = "the stand-in collector serves OTLP, not the API's documented routes"
        )]
        let router = Router::new()
            .route("/v1/traces", post(traces))
            .route("/v1/logs", post(logs))
            .route("/v1/metrics", post(metrics))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                crate::errors::ignore(&error, "test_collector_stopped");
            }
        });
        Ok(Self {
            address,
            state,
            server,
        })
    }

    /// The endpoint to give the API as `OTEL_EXPORTER_OTLP_ENDPOINT`.
    pub fn endpoint(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Everything received so far.
    pub fn received(&self) -> Received {
        self.state
            .received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Waits until what was received satisfies `done`, and returns it.
    pub async fn wait_for(&self, done: impl Fn(&Received) -> bool) -> Received {
        loop {
            let changed = self.state.changed.notified();
            let received = self.received();
            if done(&received) {
                return received;
            }
            changed.await;
        }
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.server.abort();
    }
}

// An export that doesn't decode is answered with 400, which the exporter counts as failed, so a
// test that expects the export finds it missing.

async fn traces(State(state): State<Arc<Shared>>, body: Bytes) -> Response {
    match ExportTraceServiceRequest::decode(body) {
        Ok(export) => state.keep(|received| received.traces.push(export)),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn logs(State(state): State<Arc<Shared>>, body: Bytes) -> Response {
    if state.refuse_logs {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match ExportLogsServiceRequest::decode(body) {
        Ok(export) => state.keep(|received| received.logs.push(export)),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

async fn metrics(State(state): State<Arc<Shared>>, body: Bytes) -> Response {
    match ExportMetricsServiceRequest::decode(body) {
        Ok(export) => state.keep(|received| received.metrics.push(export)),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

impl Shared {
    /// Keeps an export, wakes whoever waits for one, and answers that it was taken whole.
    fn keep(&self, store: impl FnOnce(&mut Received)) -> Response {
        store(&mut self.received.lock().unwrap_or_else(PoisonError::into_inner));
        self.changed.notify_waiters();
        accepted()
    }
}

/// The answer to an export the collector took whole: an empty response message.
fn accepted() -> Response {
    (
        [(header::CONTENT_TYPE, "application/x-protobuf")],
        Bytes::new(),
    )
        .into_response()
}
