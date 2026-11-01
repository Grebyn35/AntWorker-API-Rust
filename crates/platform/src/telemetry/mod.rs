//! The logging and tracing setup (OBS-01, OBS-02), the export of telemetry over OTLP
//! (PLATFORM-039), the metric declarations (OBS-11), the trace context of the current request,
//! and the instance's own memory (PLATFORM-046).

mod export;
pub mod metrics;
pub mod names;
pub mod process;

use std::{io, panic::PanicHookInfo};

use json_subscriber::JsonLayer;
use opentelemetry::trace::{
    SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState, TracerProvider,
};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::{
    logs::SdkLoggerProvider,
    metrics::SdkMeterProvider,
    trace::{Sampler, SdkTracerProvider},
};
use tracing::{Span, Subscriber};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{
    Layer,
    filter::{self, LevelFilter, Targets},
    fmt::MakeWriter,
    layer::SubscriberExt,
    registry::LookupSpan,
};

pub use export::{ExportError, OtlpEndpoint};

use self::metrics::Counter;
use crate::config::{Environment, LogLevel, TelemetryConfig};

/// The lines the logging thread holds before an event waits for room. An event never waits for
/// stdout itself, and no line is dropped while the process runs (CODE-05, OBS-08).
const QUEUED_LINES: usize = 128_000;

static PANICS: Counter<0> = Counter::new(
    "platform.panics",
    "{panic}",
    "Panics, wherever they happened (ERR-12).",
    [],
);

/// What identifies the service in every JSON line and every exported span, log record and
/// metric (OBS-02).
#[derive(Clone, Copy, Debug)]
pub struct Service {
    /// The binary's name, which `just rename` renames (ARCH-15).
    pub name: &'static str,
    /// The binary's version.
    pub version: &'static str,
}

/// The installed telemetry. [`Telemetry::finish`] writes out what it holds.
#[derive(Debug)]
pub struct Telemetry {
    writer: WorkerGuard,
    tracer: SdkTracerProvider,
    logs: Option<SdkLoggerProvider>,
    metrics: Option<SdkMeterProvider>,
}

impl Telemetry {
    /// Writes out the spans, then the log records, then the metrics not yet exported, so the last
    /// metrics count the exports just before them, and then the queued log lines (PLATFORM-042).
    /// The tracer and the logger each have 2 seconds, the meter the 5 seconds its SDK allows
    /// itself, and the logging thread about a second. `main` calls it last.
    pub fn finish(self) {
        if let Err(error) = self.tracer.shutdown_with_timeout(export::FLUSH_TIMEOUT) {
            crate::errors::ignore(&error, "tracer_shutdown");
        }
        if let Some(logs) = self.logs
            && let Err(error) = logs.shutdown_with_timeout(export::FLUSH_TIMEOUT)
        {
            crate::errors::ignore(&error, "logger_shutdown");
        }
        if let Some(metrics) = self.metrics
            && let Err(error) = metrics.shutdown()
        {
            crate::errors::ignore(&error, "meter_shutdown");
        }
        drop(self.writer);
    }
}

/// Telemetry couldn't be set up.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// Another logger is already installed.
    #[error("a logger is already installed")]
    Installed,
    /// The exporters couldn't be built (OBS-10).
    #[error("telemetry export couldn't be set up")]
    Export(#[from] ExportError),
}

/// Installs the process-wide logger and tracer, the panic hook, and with an endpoint the export
/// of spans, log records and metrics over OTLP (PLATFORM-039). Lines go to stdout through a
/// thread of their own (CODE-05). Call it before the async runtime starts, since the exporters'
/// HTTP client starts a runtime of its own.
pub fn init(service: Service, config: &TelemetryConfig) -> Result<Telemetry, TelemetryError> {
    let exporting = exporting(service, config)?;
    let (writer, guard) = NonBlockingBuilder::default()
        .lossy(false)
        .buffered_lines_limit(QUEUED_LINES)
        .thread_name("logging")
        .finish(io::stdout());
    let level = level_filter(config.level);
    let only_ours = lines_filter(service, level, &[]);
    let lines = match config.environment {
        Environment::Production => json(service, config.environment, writer)
            .with_filter(only_ours.clone())
            .boxed(),
        Environment::Development | Environment::Test => tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_filter(only_ours.clone())
            .boxed(),
    };
    let mut tracer = SdkTracerProvider::builder().with_sampler(Sampler::AlwaysOn);
    let (logs, metrics) = match exporting {
        Some(exporting) => {
            tracer = tracer
                .with_resource(exporting.resource)
                .with_span_processor(exporting.spans);
            (Some(exporting.logs), Some(exporting.metrics))
        }
        None => (None, None),
    };
    let tracer = tracer.build();
    let records = logs
        .as_ref()
        .map(|logs| OpenTelemetryTracingBridge::new(logs).with_filter(only_ours));
    let subscriber = tracing_subscriber::registry()
        .with(spans(&tracer))
        .with(lines)
        .with(records);
    tracing::subscriber::set_global_default(subscriber).map_err(|_| TelemetryError::Installed)?;
    if let Some(metrics) = &metrics {
        opentelemetry::global::set_meter_provider(metrics.clone());
    }
    self::metrics::provided();
    std::panic::set_hook(Box::new(log_panic));
    Ok(Telemetry {
        writer: guard,
        tracer,
        logs,
        metrics,
    })
}

/// The exporters for the configured endpoint, with the providers that batch for them, or `None`
/// without an endpoint, when nothing is exported (PLATFORM-039).
fn exporting(
    service: Service,
    config: &TelemetryConfig,
) -> Result<Option<export::Exporting>, ExportError> {
    config
        .otlp_endpoint
        .as_ref()
        .map(|endpoint| export::build(service, config.environment, endpoint))
        .transpose()
}

/// The events the log lines and records take: those of the workspace's own crates, `platform`,
/// `app` and the binary, and of `also`, at `level` and above. Other crates' events name their
/// fields outside the list and don't keep their messages constant, so they aren't written
/// (OBS-01, OBS-05). The exporters' own events stay out, so exporting never feeds itself.
fn lines_filter(service: Service, level: LevelFilter, also: &[&str]) -> Targets {
    ["platform", "app", &service.name.replace('-', "_")]
        .into_iter()
        .chain(also.iter().copied())
        .fold(Targets::new(), |targets, target| {
            targets.with_target(target, level)
        })
}

/// The layer that turns `INFO` spans and above into OpenTelemetry spans, whatever level the
/// logs are written at, so every request has a trace context. A span's attributes are its own
/// fields, all from the list of names (OBS-05): the bridge adds no source location, thread,
/// target, level or timings. Events stay out of spans, since each is a log record that its trace
/// context links to its span.
fn spans<S>(tracer: &SdkTracerProvider) -> impl Layer<S>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    tracing_opentelemetry::layer()
        .with_tracer(tracer.tracer("platform"))
        .with_location(false)
        .with_threads(false)
        .with_target(false)
        .with_level(false)
        .with_tracked_inactivity(false)
        .with_error_fields_to_exceptions(false)
        .with_error_records_to_exceptions(false)
        .with_error_events_to_exceptions(false)
        .with_error_events_to_status(false)
        .with_filter(filter::filter_fn(|metadata| {
            metadata.is_span() && *metadata.level() <= tracing::Level::INFO
        }))
}

/// One JSON object per line, with the event's fields at the top level next to `timestamp`,
/// `level` and `message`, the service's fields, and the trace context of the span the event
/// was written in (OBS-02, OBS-05).
pub(crate) fn json<S, W>(service: Service, environment: Environment, writer: W) -> JsonLayer<S, W>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let mut layer = JsonLayer::new(writer);
    layer
        .with_timer("timestamp", tracing_subscriber::fmt::time::SystemTime)
        .with_level("level")
        .with_flattened_event();
    layer.add_static_field(names::SERVICE_NAME, service.name.into());
    layer.add_static_field(names::SERVICE_VERSION, service.version.into());
    layer.add_static_field(
        names::DEPLOYMENT_ENVIRONMENT_NAME,
        environment_name(environment).into(),
    );
    // json-subscriber's own OpenTelemetry option nests the IDs in an object, and OBS-05 wants
    // them at the top level.
    layer.add_dynamic_field(names::TRACE_ID, |_, context| {
        context_ids(context).map(|(trace, _)| trace)
    });
    layer.add_dynamic_field(names::SPAN_ID, |_, context| {
        context_ids(context).map(|(_, span)| span)
    });
    layer
}

/// The trace and span IDs of the span an event is written in.
fn context_ids<S>(context: &tracing_subscriber::layer::Context<'_, S>) -> Option<(String, String)>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    let span = context.lookup_current()?;
    let otel = tracing::dispatcher::get_default(|dispatch| {
        tracing_opentelemetry::get_otel_context(&span.id(), dispatch)
    })?;
    let otel_span = otel.span();
    let ids = otel_span.span_context();
    ids.is_valid()
        .then(|| (ids.trace_id().to_string(), ids.span_id().to_string()))
}

/// The value of `deployment.environment.name` for `environment`.
pub fn environment_name(environment: Environment) -> &'static str {
    match environment {
        Environment::Development => "development",
        Environment::Test => "test",
        Environment::Production => "production",
    }
}

/// The trace ID of the current span, as 32 hexadecimal digits.
pub fn trace_id() -> String {
    Span::current()
        .context()
        .span()
        .span_context()
        .trace_id()
        .to_string()
}

/// The span's trace context as a W3C `traceparent` value, or `None` when it has none.
pub(crate) fn traceparent(span: &Span) -> Option<String> {
    let context = span.context();
    let otel_span = context.span();
    let ids = otel_span.span_context();
    ids.is_valid().then(|| {
        format!(
            "00-{}-{}-{:02x}",
            ids.trace_id(),
            ids.span_id(),
            ids.trace_flags().to_u8()
        )
    })
}

/// Links `span` to the span whose trace context `traceparent` holds, as [`traceparent`] wrote
/// it, so work that happens later leads back to what caused it (OBS-07). A value that isn't a
/// valid trace context adds no link.
pub(crate) fn link(span: &Span, traceparent: &str) {
    let mut parts = traceparent.split('-');
    let (Some("00"), Some(trace), Some(parent), Some(flags), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return;
    };
    let (Ok(trace), Ok(parent), Ok(flags)) = (
        TraceId::from_hex(trace),
        SpanId::from_hex(parent),
        u8::from_str_radix(flags, 16),
    ) else {
        return;
    };
    let context = SpanContext::new(
        trace,
        parent,
        TraceFlags::new(flags),
        true,
        TraceState::default(),
    );
    if context.is_valid() {
        span.add_link(context);
    }
}

/// Writes one `ERROR` line for a panic, in the span it happened in, instead of the default
/// hook's plain text on stderr, and counts it (ERR-12).
fn log_panic(info: &PanicHookInfo<'_>) {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_default();
    let (file, line) = info
        .location()
        .map_or(("", 0), |location| (location.file(), location.line()));
    tracing::error!(
        { names::EXCEPTION_MESSAGE } = message,
        { names::CODE_FILE_PATH } = file,
        { names::CODE_LINE_NUMBER } = line,
        "panicked"
    );
    PANICS.add(1, []);
}

#[cfg(feature = "test-support")]
pub mod testing;

fn level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_039_without_the_setting_no_exporter_is_built() {
        let config = TelemetryConfig {
            environment: Environment::Production,
            level: LogLevel::Info,
            otlp_endpoint: None,
        };
        let service = Service {
            name: "api",
            version: "0.0.0",
        };

        assert!(exporting(service, &config).unwrap().is_none());
    }
}
