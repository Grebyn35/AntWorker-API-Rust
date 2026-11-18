//! The metric `client_telemetry` declares (OBS-11).

use platform::telemetry::{Signal, metrics::Counter, names};

/// The bytes of the batches that the collector took from the proxy, by signal.
static FORWARDED: Counter<1> = Counter::new(
    "client_telemetry.forwarded",
    "By",
    "The bytes, inflated, of the batches of clients' telemetry that the collector took from the \
     telemetry proxy, by the signal of each: traces, logs or metrics (CLIENT_TELEMETRY-013).",
    [names::CLIENT_TELEMETRY_SIGNAL],
);

/// Counts the `bytes` of a batch of `signal` that the collector took.
pub(super) fn forwarded(signal: Signal, bytes: u32) {
    let label = match signal {
        Signal::Traces => "traces",
        Signal::Logs => "logs",
        Signal::Metrics => "metrics",
    };
    FORWARDED.add(u64::from(bytes), [label.into()]);
}
