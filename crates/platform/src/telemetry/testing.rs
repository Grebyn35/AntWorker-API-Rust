//! The telemetry of the tests, which compile with the `test-support` feature.

pub mod collector;

use std::{
    cell::RefCell,
    collections::BTreeMap,
    io,
    sync::{
        Mutex, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, ThreadId},
    time::Duration,
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

/// Every line the process's logger has written, and the watches of the tests that run.
static LOGGED: Mutex<Logged> = Mutex::new(Logged {
    lines: Vec::new(),
    watches: Vec::new(),
});

/// Numbers the watches.
static NEXT_WATCH: AtomicU64 = AtomicU64::new(0);

/// How long a test runs before its [`Watch`] prints what it logged: longer than the whole suite
/// of integration tests takes, and well inside the time limit of CI's job.
const PRINTED_AFTER: Duration = Duration::from_secs(300);

struct Logged {
    /// Each write of the logger, which is one line, with the thread it was written on.
    lines: Vec<(ThreadId, String)>,
    watches: Vec<Watched>,
}

/// The test that a [`Watch`] watches.
struct Watched {
    watch: u64,
    /// The thread the test runs on, whose lines are the test's.
    thread: ThreadId,
    test: String,
    /// Where the lines go once they're printed.
    output: Box<dyn io::Write + Send>,
    printing: bool,
}

impl Watched {
    /// Writes `line` to the output, after the test's name.
    fn print(&mut self, line: &str) -> io::Result<()> {
        for line in line.lines() {
            writeln!(self.output, "{}: {line}", self.test)?;
        }
        self.output.flush()
    }
}

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

thread_local! {
    /// The watch of the test that runs on this thread, which ends with the thread, as the test
    /// runner runs each test on a thread of its own.
    static WATCHED: RefCell<Option<Watch>> = const { RefCell::new(None) };
}

/// Installs the API's tracer, its JSON logger at `INFO` writing into memory, a meter whose
/// metrics stay in memory, and the API's panic hook, as the process's. The hook then runs the
/// test runner's, which prints a failing test's message. Requests that tests send get the trace
/// context the API gives them, and a test reads the lines of its own requests by their trace
/// ID, whatever other tests run alongside. Later calls install nothing. Each call watches the
/// test on the thread it runs on, once, until the thread ends (TEST-07). Fails if something else
/// installed a subscriber first.
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
        let runners = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            super::log_panic(info);
            runners(info);
        }));
        Some(Installed {
            _tracer: tracer,
            meter,
            metrics,
        })
    });
    installed.as_ref().ok_or(TelemetryError::Installed)?;
    WATCHED.with(|watched| {
        watched
            .borrow_mut()
            .get_or_insert_with(|| Watch::printing_to(PRINTED_AFTER, io::stderr()));
    });
    Ok(())
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
    let logged = LOGGED.lock().unwrap_or_else(PoisonError::into_inner);
    logged
        .lines
        .iter()
        .flat_map(|(_, written)| written.lines())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Prints what the test that starts it has logged, once the watch has lived for a while, then
/// each line the test logs, until the watch is dropped, so that a test that never ends shows
/// what it logged before CI's job time limit ends it (TEST-07). [`install`] starts one for 5
/// minutes. Each printed line starts with the test's name. A test's lines are those written on
/// the thread the test runner runs it on, where its runtime runs its tasks, and not those of the
/// threads that run blocking work.
#[must_use = "the watch ends when it's dropped"]
pub struct Watch {
    watch: u64,
    /// Ends the thread that waits to print, once dropped.
    _ended: mpsc::Sender<()>,
}

impl Watch {
    /// Watches the test that runs on this thread, printing to `output` after `after`.
    pub fn printing_to(after: Duration, output: impl io::Write + Send + 'static) -> Self {
        let watch = NEXT_WATCH.fetch_add(1, Ordering::Relaxed);
        let current = thread::current();
        let test = current.name().unwrap_or("a test").to_owned();
        LOGGED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .watches
            .push(Watched {
                watch,
                thread: current.id(),
                test,
                output: Box::new(output),
                printing: false,
            });
        let (ended, waiting) = mpsc::channel::<()>();
        thread::spawn(move || {
            #[expect(
                clippy::disallowed_methods,
                reason = "the watch prints a hung test's lines at a time on the real clock, \
                          and decides no test's outcome (TEST-07)"
            )]
            let outcome = waiting.recv_timeout(after);
            if outcome == Err(mpsc::RecvTimeoutError::Timeout)
                && let Err(error) = print_logged(watch, after)
            {
                crate::errors::ignore(&error, "test_lines_not_printed");
            }
        });
        Self {
            watch,
            _ended: ended,
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        LOGGED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .watches
            .retain(|watched| watched.watch != self.watch);
    }
}

/// Prints what the test of `watch` has logged, unless another watch of its thread already
/// does, and marks the watch as printing each line its test logs from now on.
fn print_logged(watch: u64, after: Duration) -> io::Result<()> {
    let mut logged = LOGGED.lock().unwrap_or_else(PoisonError::into_inner);
    let Logged { lines, watches } = &mut *logged;
    let Some(index) = watches.iter().position(|watched| watched.watch == watch) else {
        return Ok(());
    };
    let thread = watches[index].thread;
    let printed = watches
        .iter()
        .any(|watched| watched.thread == thread && watched.printing);
    let watched = &mut watches[index];
    watched.printing = true;
    if printed {
        return Ok(());
    }
    writeln!(
        watched.output,
        "{} has run for {} seconds; what it logged follows, then each line it logs",
        watched.test,
        after.as_secs()
    )?;
    for (_, line) in lines.iter().filter(|(written, _)| *written == thread) {
        watched.print(line)?;
    }
    Ok(())
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

/// Writes into [`LOGGED`], and prints each line that a watch prints.
#[derive(Clone, Copy)]
struct Memory;

impl io::Write for Memory {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let thread = thread::current().id();
        let line = String::from_utf8_lossy(bytes).into_owned();
        let mut logged = LOGGED.lock().unwrap_or_else(PoisonError::into_inner);
        // A line that can't be printed is still kept, and the logger reports the error.
        let printed = logged
            .watches
            .iter_mut()
            .find(|watched| watched.thread == thread && watched.printing)
            .map_or(Ok(()), |watched| watched.print(&line));
        logged.lines.push((thread, line));
        printed.map(|()| bytes.len())
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
