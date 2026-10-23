//! The logging and tracing setup (OBS-01, OBS-02), and the trace context of the current
//! request.

pub mod names;

use std::{io, panic::PanicHookInfo};

use json_subscriber::JsonLayer;
use opentelemetry::trace::{TraceContextExt, TracerProvider};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tracing::{Span, Subscriber};
use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{
    Layer, filter::LevelFilter, fmt::MakeWriter, layer::SubscriberExt, registry::LookupSpan,
};

use crate::config::{Environment, LogLevel, LoggingConfig};

/// The lines the logging thread holds before an event waits for room. An event never waits for
/// stdout itself, and no line is dropped (CODE-05, OBS-08).
const QUEUED_LINES: usize = 128_000;

/// What identifies the service in every JSON line (OBS-02).
#[derive(Clone, Copy, Debug)]
pub struct Service {
    /// The binary's name, which `just rename` renames (ARCH-15).
    pub name: &'static str,
    /// The binary's version.
    pub version: &'static str,
}

/// The installed telemetry. [`Telemetry::finish`] writes out the queued log lines.
#[derive(Debug)]
pub struct Telemetry {
    writer: WorkerGuard,
    tracer: SdkTracerProvider,
}

impl Telemetry {
    /// Ends the tracer, then writes out every queued log line. `main` calls it last.
    pub fn finish(self) {
        // The tracer exports nothing yet, so ending it can't lose anything worth reporting.
        self.tracer.shutdown().ok();
        drop(self.writer);
    }
}

/// Telemetry couldn't be set up, because another logger is already installed.
#[derive(Debug, thiserror::Error)]
#[error("a logger is already installed")]
pub struct TelemetryError;

/// Installs the process-wide logger and tracer, and the panic hook. Lines go to stdout through
/// a thread of their own (CODE-05).
pub fn init(service: Service, config: &LoggingConfig) -> Result<Telemetry, TelemetryError> {
    let (writer, guard) = NonBlockingBuilder::default()
        .lossy(false)
        .buffered_lines_limit(QUEUED_LINES)
        .thread_name("logging")
        .finish(io::stdout());
    let tracer = tracer();
    let spans = spans(&tracer);
    let level = level_filter(config.level);
    let lines = match config.environment {
        Environment::Production => json(service, config.environment, writer)
            .with_filter(level)
            .boxed(),
        Environment::Development | Environment::Test => tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_filter(level)
            .boxed(),
    };
    let subscriber = tracing_subscriber::registry().with(spans).with(lines);
    tracing::subscriber::set_global_default(subscriber).map_err(|_| TelemetryError)?;
    std::panic::set_hook(Box::new(log_panic));
    Ok(Telemetry {
        writer: guard,
        tracer,
    })
}

/// The tracer: OpenTelemetry's SDK, sampling every request whatever an inbound `traceparent`
/// says (OBS-08). It exports nothing until telemetry export is built.
fn tracer() -> SdkTracerProvider {
    SdkTracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .build()
}

/// The layer that turns `INFO` spans and above into OpenTelemetry spans, whatever level the
/// logs are written at, so every request has a trace context.
fn spans<S>(tracer: &SdkTracerProvider) -> impl Layer<S>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    tracing_opentelemetry::layer()
        .with_tracer(tracer.tracer("platform"))
        .with_filter(LevelFilter::INFO)
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
pub(crate) fn trace_id() -> String {
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

/// Writes one `ERROR` line for a panic, in the span it happened in, instead of the default
/// hook's plain text on stderr (ERR-12).
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
}

/// The telemetry of the tests, which compile with the `test-support` feature.
#[cfg(feature = "test-support")]
pub mod testing {
    use std::{
        io,
        sync::{Mutex, OnceLock, PoisonError},
    };

    use opentelemetry_sdk::trace::SdkTracerProvider;
    use serde_json::Value;
    use tracing_subscriber::{Layer, filter::LevelFilter, fmt::MakeWriter, layer::SubscriberExt};

    use super::{Service, names};
    use crate::config::Environment;

    /// Every line the process's logger has written.
    static LINES: Mutex<Vec<u8>> = Mutex::new(Vec::new());

    /// Installs the API's tracer, and its JSON logger at `INFO` writing into memory, as the
    /// process's subscriber. Requests that tests send get the trace context the API gives them,
    /// and a test reads the lines of its own requests by their trace ID, whatever other tests
    /// run alongside. Later calls do nothing.
    pub fn install(service: Service) {
        static TRACER: OnceLock<SdkTracerProvider> = OnceLock::new();
        TRACER.get_or_init(|| {
            let tracer = super::tracer();
            let subscriber = tracing_subscriber::registry()
                .with(super::spans(&tracer))
                .with(
                    super::json(service, Environment::Test, Memory).with_filter(LevelFilter::INFO),
                );
            // Nothing else in a test process installs a subscriber.
            tracing::subscriber::set_global_default(subscriber).ok();
            tracer
        });
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

    /// Installs the API's panic hook (ERR-12). It replaces the hook that prints a failing test's
    /// message, so only a test binary of its own installs it.
    pub fn install_panic_hook() {
        std::panic::set_hook(Box::new(super::log_panic));
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
}

fn level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Error => LevelFilter::ERROR,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Trace => LevelFilter::TRACE,
    }
}
