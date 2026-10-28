//! The telemetry of the tests, which compile with the `test-support` feature.

pub mod collector;

use std::{
    collections::BTreeMap,
    io,
    sync::{Mutex, OnceLock, PoisonError},
};

use opentelemetry_sdk::{
    metrics::{
        InMemoryMetricExporter, PeriodicReader, SdkMeterProvider,
        data::{AggregatedMetrics, MetricData},
    },
    trace::SdkTracerProvider,
};
use serde_json::Value;
use tracing_subscriber::{Layer, filter::LevelFilter, fmt::MakeWriter, layer::SubscriberExt};

use super::{Service, TelemetryError, names};
use crate::config::Environment;

/// Every line the process's logger has written.
static LINES: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// The test binaries, whose own events the tests' logger writes too.
const TEST_BINARIES: [&str; 1] = ["integration"];

/// What [`install`] installed: the tracer, and the meter with the exporter that keeps its
/// metrics in memory.
struct Installed {
    _tracer: SdkTracerProvider,
    meter: SdkMeterProvider,
    metrics: InMemoryMetricExporter,
}

static INSTALLED: OnceLock<Option<Installed>> = OnceLock::new();

/// Installs the API's tracer, its JSON logger at `INFO` writing into memory, and a meter whose
/// metrics stay in memory, as the process's. Requests that tests send get the trace context the
/// API gives them, and a test reads the lines of its own requests by their trace ID, whatever
/// other tests run alongside. Later calls do nothing. Fails if something else installed a
/// subscriber first.
pub fn install(service: Service) -> Result<(), TelemetryError> {
    let installed = INSTALLED.get_or_init(|| {
        let tracer = SdkTracerProvider::builder()
            .with_sampler(opentelemetry_sdk::trace::Sampler::AlwaysOn)
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(super::spans(&tracer))
            .with(super::json(service, Environment::Test, Memory).with_filter(
                super::lines_filter(service, LevelFilter::INFO, &TEST_BINARIES),
            ));
        tracing::subscriber::set_global_default(subscriber).ok()?;
        let metrics = InMemoryMetricExporter::default();
        let meter = SdkMeterProvider::builder()
            .with_resource(super::export::resource(service, Environment::Test))
            .with_reader(PeriodicReader::builder(metrics.clone()).build())
            .build();
        opentelemetry::global::set_meter_provider(meter.clone());
        super::metrics::provided();
        Some(Installed {
            _tracer: tracer,
            meter,
            metrics,
        })
    });
    installed
        .as_ref()
        .map(|_| ())
        .ok_or(TelemetryError::Installed)
}

/// Every line logged in the trace `trace_id`, in order.
pub fn logged(trace_id: &str) -> Vec<Value> {
    all_logged()
        .into_iter()
        .filter(|line| line[names::TRACE_ID] == trace_id)
        .collect()
}

/// Every line logged so far, in order.
pub fn all_logged() -> Vec<Value> {
    let lines = LINES.lock().unwrap_or_else(PoisonError::into_inner);
    String::from_utf8_lossy(&lines)
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// One data point of a metric: its labels, and its value. A histogram's value is the sum of what
/// it recorded, and its `count` how many recordings it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    /// Each label, by name.
    pub labels: BTreeMap<String, String>,
    /// A counter's or a gauge's value, or a histogram's sum.
    pub value: f64,
    /// A histogram's number of recordings, or `None` for a counter or a gauge.
    pub count: Option<u64>,
}

/// Records what `read` reports as the process's memory, now and every 15 seconds after on
/// `clock`, as `process::watch` records the kernel's report, which a test can't choose (TEST-05).
pub fn watch_memory<F, Report>(clock: crate::clock::SharedClock, read: F)
where
    F: Fn() -> Report + Send + 'static,
    Report: Future<Output = Option<u64>> + Send + 'static,
{
    super::process::watch_reading(clock, read);
}

/// The data points of the metric `name`, as everything recorded in the process so far adds up
/// to. Tests running alongside record too, so a test reads the points whose labels only it
/// records. Empty if [`install`] hasn't run or nothing recorded the metric.
pub fn recorded(name: &str) -> Vec<Point> {
    let Some(Some(installed)) = INSTALLED.get() else {
        return Vec::new();
    };
    // A flush that fails leaves the last export, which is still what was recorded before it.
    if let Err(error) = installed.meter.force_flush() {
        crate::errors::ignore(&error, "test_metrics_not_flushed");
    }
    let exports = installed
        .metrics
        .get_finished_metrics()
        .unwrap_or_else(|error| {
            crate::errors::ignore(&error, "test_metrics_not_read");
            Vec::new()
        });
    let Some(last) = exports.last() else {
        return Vec::new();
    };
    last.scope_metrics()
        .flat_map(|scope| scope.metrics())
        .filter(|metric| metric.name() == name)
        .flat_map(|metric| match metric.data() {
            AggregatedMetrics::F64(data) => points(data, |value| value),
            AggregatedMetrics::U64(data) => points(data, to_f64),
            AggregatedMetrics::I64(data) => points(data, |value| to_f64(value.unsigned_abs())),
        })
        .collect()
}

/// `value` as a float. The tests' counts are far below 2^52, so none loses precision.
fn to_f64(value: u64) -> f64 {
    value as f64
}

fn points<T: Copy>(data: &MetricData<T>, value: impl Fn(T) -> f64) -> Vec<Point> {
    let labels = |attributes: &mut dyn Iterator<Item = &opentelemetry::KeyValue>| {
        attributes
            .map(|pair| (pair.key.to_string(), pair.value.to_string()))
            .collect()
    };
    match data {
        MetricData::Sum(sum) => sum
            .data_points()
            .map(|point| Point {
                labels: labels(&mut point.attributes()),
                value: value(point.value()),
                count: None,
            })
            .collect(),
        MetricData::Gauge(gauge) => gauge
            .data_points()
            .map(|point| Point {
                labels: labels(&mut point.attributes()),
                value: value(point.value()),
                count: None,
            })
            .collect(),
        MetricData::Histogram(histogram) => histogram
            .data_points()
            .map(|point| Point {
                labels: labels(&mut point.attributes()),
                value: value(point.sum()),
                count: Some(point.count()),
            })
            .collect(),
        MetricData::ExponentialHistogram(_) => Vec::new(),
    }
}

/// Writes into [`LINES`].
#[derive(Clone, Copy)]
struct Memory;

impl io::Write for Memory {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        LINES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MakeWriter<'_> for Memory {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        *self
    }
}
