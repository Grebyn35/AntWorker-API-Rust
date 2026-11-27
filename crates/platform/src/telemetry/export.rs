//! The export of spans, log records and metrics over OTLP/HTTP (PLATFORM-066), and the counts of
//! what each exporter exported (OBS-10).

use std::{fmt, str::FromStr, time::Duration};

use opentelemetry::{Key, KeyValue};
use opentelemetry_otlp::{
    ExporterBuildError, LogExporter, MetricExporter, Protocol, SpanExporter, WithExportConfig,
    WithHttpConfig,
};
use opentelemetry_sdk::{
    Resource,
    error::OTelSdkResult,
    logs::{BatchLogProcessor, LogBatch, LogExporter as ExportsLogs, SdkLoggerProvider},
    metrics::{
        Aggregation, Instrument, PeriodicReader, SdkMeterProvider, Stream, Temporality,
        data::{AggregatedMetrics, MetricData, ResourceMetrics},
        exporter::PushMetricExporter,
    },
    trace::{BatchConfigBuilder, BatchSpanProcessor, SpanData, SpanExporter as ExportsSpans},
};
use opentelemetry_semantic_conventions::metric as otel;
use url::Url;

use super::{
    Service, environment_name,
    metrics::{Counter, LabelValue},
    names::{self, convention},
};
use crate::config::Environment;

/// How often the queued spans and log records are exported, at the least.
const EXPORT_DELAY: Duration = Duration::from_secs(1);

/// The most spans or log records one export sends.
const EXPORT_BATCH: usize = 512;

/// The spans, or log records, a queue holds before it drops what arrives.
const QUEUE: usize = 2_048;

/// How often the metrics are exported.
const METRIC_INTERVAL: Duration = Duration::from_secs(15);

/// How long an export may take before it fails.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the tracer and the logger each have to write out what they hold at shutdown. The
/// meter's SDK allows itself 5 seconds.
pub(super) const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// The scope of the metrics that the SDK counts itself.
const SDK_SCOPE: &str = "otel.sdk";

/// The metrics that count what the processors of spans and of log records hand on and drop,
/// which [`sdk_view`] keeps of the SDK's own.
const PROCESSED: [&str; 2] = [
    convention(
        "otel.sdk.processor.span.processed",
        otel::OTEL_SDK_PROCESSOR_SPAN_PROCESSED,
    ),
    convention(
        "otel.sdk.processor.log.processed",
        otel::OTEL_SDK_PROCESSOR_LOG_PROCESSED,
    ),
];

static SPANS_EXPORTED: Counter<1> = Counter::new(
    convention(
        "otel.sdk.exporter.span.exported",
        otel::OTEL_SDK_EXPORTER_SPAN_EXPORTED,
    ),
    "{span}",
    "The spans whose export finished, with error.type when it failed.",
    [names::ERROR_TYPE],
);

static LOGS_EXPORTED: Counter<1> = Counter::new(
    convention(
        "otel.sdk.exporter.log.exported",
        otel::OTEL_SDK_EXPORTER_LOG_EXPORTED,
    ),
    "{log_record}",
    "The log records whose export finished, with error.type when it failed.",
    [names::ERROR_TYPE],
);

static DATA_POINTS_EXPORTED: Counter<1> = Counter::new(
    convention(
        "otel.sdk.exporter.metric_data_point.exported",
        otel::OTEL_SDK_EXPORTER_METRIC_DATA_POINT_EXPORTED,
    ),
    "{data_point}",
    "The metric data points whose export finished, with error.type when it failed.",
    [names::ERROR_TYPE],
);

/// Where telemetry is exported: the base URL of an OTLP/HTTP receiver, such as the collector's
/// `http://collector:4318`, to which each signal's path is added.
#[derive(Clone, PartialEq, Eq)]
pub struct OtlpEndpoint(Url);

impl OtlpEndpoint {
    /// The URL of one signal's path under the endpoint, such as `v1/traces`.
    fn signal(&self, path: &str) -> String {
        format!("{}/{path}", self.0.as_str().trim_end_matches('/'))
    }
}

// part: client_telemetry
/// What the relay of clients' batches reads of the endpoint.
impl OtlpEndpoint {
    /// The URL of `path` under the endpoint.
    pub(super) fn url(&self, path: &str) -> String {
        self.signal(path)
    }

    /// Whether the endpoint is `https`.
    pub(super) fn is_https(&self) -> bool {
        self.0.scheme() == "https"
    }

    /// The endpoint's host.
    pub(super) fn host(&self) -> String {
        self.0.host_str().unwrap_or_default().to_owned()
    }
}
// end part: client_telemetry

impl FromStr for OtlpEndpoint {
    type Err = ();

    /// An `http` or `https` URL with a host, and no user, password, query or fragment, which
    /// would put a secret where telemetry's settings are logged, or change the signals' paths.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let url = Url::parse(value).map_err(|_| ())?;
        let usable = matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|host| !host.is_empty())
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none();
        usable.then_some(Self(url)).ok_or(())
    }
}

impl fmt::Debug for OtlpEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("OtlpEndpoint")
            .field(&self.0.as_str())
            .finish()
    }
}

/// The exporters couldn't be built.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The HTTP client couldn't be built, such as when the certificate store can't be read.
    #[error("the exporters' HTTP client couldn't be built")]
    Client(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// An exporter refused its configuration.
    #[error("an exporter couldn't be built")]
    Exporter(#[source] ExporterBuildError),
    /// The view of the metrics that the SDK counts itself couldn't be built.
    #[error("the view of the SDK's own metrics couldn't be built: {0}")]
    View(String),
}

/// The providers that export, built, of which only the meter provider is installed yet.
pub(super) struct Exporting {
    pub(super) spans: BatchSpanProcessor,
    pub(super) logs: SdkLoggerProvider,
    pub(super) metrics: SdkMeterProvider,
    pub(super) resource: Resource,
}

/// Builds the exporters for `endpoint` and the providers that batch for them, and installs the
/// meter provider as the process's before it builds the processors, which count what they drop
/// in it (the platform spec's Telemetry export).
pub(super) fn build(
    service: Service,
    environment: Environment,
    endpoint: &OtlpEndpoint,
) -> Result<Exporting, ExportError> {
    let client = client(endpoint)?;
    let resource = resource(service, environment);
    let spans = SpanExporter::builder()
        .with_http()
        .with_http_client(client.clone())
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(endpoint.signal("v1/traces"))
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(ExportError::Exporter)?;
    let logs = LogExporter::builder()
        .with_http()
        .with_http_client(client.clone())
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(endpoint.signal("v1/logs"))
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(ExportError::Exporter)?;
    let metrics = MetricExporter::builder()
        .with_http()
        .with_http_client(client)
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(endpoint.signal("v1/metrics"))
        .with_timeout(EXPORT_TIMEOUT)
        .with_temporality(Temporality::Cumulative)
        .build()
        .map_err(ExportError::Exporter)?;
    let batches = BatchConfigBuilder::default()
        .with_scheduled_delay(EXPORT_DELAY)
        .with_max_export_batch_size(EXPORT_BATCH)
        .with_max_queue_size(QUEUE)
        .build();
    let log_batches = opentelemetry_sdk::logs::BatchConfigBuilder::default()
        .with_scheduled_delay(EXPORT_DELAY)
        .with_max_export_batch_size(EXPORT_BATCH)
        .with_max_queue_size(QUEUE)
        .build();
    for kept in [true, false] {
        sdk_stream(kept).map_err(|error| ExportError::View(error.to_string()))?;
    }
    let metrics = SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_view(sdk_view)
        .with_reader(
            PeriodicReader::builder(CountedMetrics(metrics))
                .with_interval(METRIC_INTERVAL)
                .build(),
        )
        .build();
    // Each processor makes its counters from the process's meter provider as it is built, so
    // the API's is the process's first.
    opentelemetry::global::set_meter_provider(metrics.clone());
    Ok(Exporting {
        spans: BatchSpanProcessor::builder(CountedSpans(spans))
            .with_batch_config(batches)
            .build(),
        logs: SdkLoggerProvider::builder()
            .with_resource(resource.clone())
            .with_log_processor(
                BatchLogProcessor::builder(CountedLogs(logs))
                    .with_batch_config(log_batches)
                    .build(),
            )
            .build(),
        metrics,
        resource,
    })
}

/// The view of the metrics that the SDK counts itself, in its scope `otel.sdk`: it keeps the two
/// that count what the processors of spans and of log records hand on and drop, each with
/// `error.type` alone of its attributes, and drops every other, so the API exports none that
/// nothing reads (OBS-12). The metrics of every other scope keep the SDK's default view.
fn sdk_view(instrument: &Instrument) -> Option<Stream> {
    if instrument.scope().name() != SDK_SCOPE {
        return None;
    }
    // `build` built both streams before it built the meter provider, so neither fails here.
    sdk_stream(PROCESSED.contains(&instrument.name())).ok()
}

/// The stream of one of the SDK's own metrics: one that keeps `error.type` alone of its
/// attributes when it is `kept`, and one that drops the metric otherwise.
fn sdk_stream(kept: bool) -> Result<Stream, Box<dyn std::error::Error>> {
    let stream = Stream::builder();
    if kept {
        stream
            .with_allowed_attribute_keys([Key::from_static_str(names::ERROR_TYPE)])
            .build()
    } else {
        stream.with_aggregation(Aggregation::Drop).build()
    }
}

/// What identifies every span, log record and metric the process exports. Nothing else is read
/// into it: the SDK's detectors, which read `OTEL_RESOURCE_ATTRIBUTES`, are left out.
#[expect(
    clippy::disallowed_methods,
    reason = "the instance's ID names this process, and no row (DATA-08)"
)]
pub(super) fn resource(service: Service, environment: Environment) -> Resource {
    Resource::builder_empty()
        .with_attributes([
            KeyValue::new(names::SERVICE_NAME, service.name),
            KeyValue::new(names::SERVICE_VERSION, service.version),
            KeyValue::new(
                names::DEPLOYMENT_ENVIRONMENT_NAME,
                environment_name(environment),
            ),
            KeyValue::new(names::SERVICE_INSTANCE_ID, uuid::Uuid::new_v4().to_string()),
        ])
        .build()
}

/// The blocking HTTP client the three exporters share. It runs on a thread of its own, so an
/// export never needs the async runtime. An `https` endpoint is verified against the operating
/// system's certificate store, as a verified database connection is (D38). It follows no
/// redirect, and reads no proxy from the environment, such as `ALL_PROXY`, so an export reaches
/// the endpoint that the settings name, or fails (SEC-10).
pub(crate) fn client(endpoint: &OtlpEndpoint) -> Result<reqwest::blocking::Client, ExportError> {
    let tls =
        crate::tls::client_config(endpoint.0.scheme() == "https").map_err(ExportError::Client)?;
    reqwest::blocking::Client::builder()
        .timeout(EXPORT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .tls_backend_preconfigured(tls)
        .build()
        .map_err(|error| ExportError::Client(Box::new(error)))
}

/// The value of `error.type` for an export's result: none on success. The OTLP exporter reports
/// every failure the same way, a timeout included, so a failure has one value.
fn outcome(result: &OTelSdkResult) -> LabelValue {
    match result {
        Ok(()) => None::<&'static str>.into(),
        Err(_) => "export_failed".into(),
    }
}

/// Counts what `n` items' export came to.
fn count(counter: &Counter<1>, items: usize, result: &OTelSdkResult) {
    counter.add(u64::try_from(items).unwrap_or(u64::MAX), [outcome(result)]);
}

/// The span exporter, counting what it exports.
#[derive(Debug)]
struct CountedSpans(SpanExporter);

impl ExportsSpans for CountedSpans {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let items = batch.len();
        let result = self.0.export(batch).await;
        count(&SPANS_EXPORTED, items, &result);
        result
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

/// The log exporter, counting what it exports.
#[derive(Debug)]
struct CountedLogs(LogExporter);

impl ExportsLogs for CountedLogs {
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        let items = batch.iter().count();
        let result = self.0.export(batch).await;
        count(&LOGS_EXPORTED, items, &result);
        result
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

/// The metric exporter, counting the data points it exports.
struct CountedMetrics(MetricExporter);

impl PushMetricExporter for CountedMetrics {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        let items = data_points(metrics);
        let result = self.0.export(metrics).await;
        count(&DATA_POINTS_EXPORTED, items, &result);
        result
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn temporality(&self) -> Temporality {
        self.0.temporality()
    }
}

/// The data points in one export of metrics.
fn data_points(metrics: &ResourceMetrics) -> usize {
    metrics
        .scope_metrics()
        .flat_map(|scope| scope.metrics())
        .map(|metric| match metric.data() {
            AggregatedMetrics::F64(data) => points(data),
            AggregatedMetrics::U64(data) => points(data),
            AggregatedMetrics::I64(data) => points(data),
        })
        .sum()
}

fn points<T>(data: &MetricData<T>) -> usize {
    match data {
        MetricData::Gauge(gauge) => gauge.data_points().count(),
        MetricData::Sum(sum) => sum.data_points().count(),
        MetricData::Histogram(histogram) => histogram.data_points().count(),
        MetricData::ExponentialHistogram(histogram) => histogram.data_points().count(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use opentelemetry::metrics::{Gauge, Histogram, MeterProvider};

    use super::*;

    // part: client_telemetry
    #[test]
    fn platform_091_the_relay_reads_certificates_for_an_https_endpoint_alone() {
        let verified = |endpoint: &str| endpoint.parse::<OtlpEndpoint>().unwrap().is_https();

        assert!(verified("https://collector.example:4318"));
        assert!(verified("HTTPS://collector.example"));
        assert!(!verified("http://collector:4318"));
    }
    // end part: client_telemetry

    /// An exporter of metrics that keeps, for each export, the data points that [`data_points`]
    /// counts in it.
    #[derive(Clone, Default)]
    struct Counting(Arc<Mutex<Vec<usize>>>);

    impl PushMetricExporter for Counting {
        async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
            self.0.lock().unwrap().push(data_points(metrics));
            Ok(())
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn temporality(&self) -> Temporality {
            Temporality::Cumulative
        }
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test records into a meter provider of its own, which no declaration reaches"
    )]
    fn obs_10_an_export_of_metrics_counts_each_of_its_data_points() {
        let exported = Counting::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exported.clone()).build())
            .build();
        let meter = provider.meter("test");
        let counter = meter.u64_counter("test.counted").build();
        for error_type in ["first", "second"] {
            counter.add(1, &[KeyValue::new(names::ERROR_TYPE, error_type)]);
        }
        // In the form of a path, which check-instrument doesn't read as a span's `record`.
        Gauge::record(&meter.u64_gauge("test.gauged").build(), 1, &[]);
        Histogram::record(&meter.f64_histogram("test.measured").build(), 1.0, &[]);

        provider.force_flush().unwrap();

        // The counter's two data points, the gauge's one, and the histogram's one.
        assert_eq!(*exported.0.lock().unwrap(), [4]);
    }

    #[test]
    fn platform_066_each_signal_has_its_path_under_the_endpoint() {
        let endpoint: OtlpEndpoint = "https://collector.internal/otlp/".parse().unwrap();
        assert_eq!(
            endpoint.signal("v1/traces"),
            "https://collector.internal/otlp/v1/traces"
        );
        let endpoint: OtlpEndpoint = "http://127.0.0.1:4318".parse().unwrap();
        assert_eq!(endpoint.signal("v1/logs"), "http://127.0.0.1:4318/v1/logs");
    }
}
