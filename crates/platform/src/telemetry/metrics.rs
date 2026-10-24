//! The metric declarations (OBS-11). A metric is a `static` built by the `const fn new` of
//! [`Counter`], [`Histogram`] or [`Gauge`], with its name, unit, description and label names.
//! `check-metrics` reads every declaration, and writes the metric catalog from them (OBS-12).
//!
//! The first recording creates the instrument from the process's meter provider, which
//! [`super::init`] sets up before any code records. Without telemetry export, the provider
//! records nothing.

use std::sync::OnceLock;

use opentelemetry::{KeyValue, Value, metrics::Meter};

/// The value of one label in a recording. A label whose value is `None` is left out of the data
/// point, as the conventions ask of `error.type` on a success.
#[derive(Clone, Debug)]
pub struct LabelValue(Option<Value>);

impl From<&'static str> for LabelValue {
    fn from(value: &'static str) -> Self {
        Self(Some(value.into()))
    }
}

impl From<String> for LabelValue {
    fn from(value: String) -> Self {
        Self(Some(value.into()))
    }
}

impl From<i64> for LabelValue {
    fn from(value: i64) -> Self {
        Self(Some(value.into()))
    }
}

impl<T: Into<Self>> From<Option<T>> for LabelValue {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self(None), Into::into)
    }
}

/// A count that only grows, such as the panics so far.
#[derive(Debug)]
pub struct Counter<const N: usize> {
    declared: Declared<N>,
    instrument: OnceLock<opentelemetry::metrics::Counter<u64>>,
}

impl<const N: usize> Counter<N> {
    /// A counter named `name`, counting in `unit`, such as `{panic}`, whose recordings carry the
    /// labels `labels`, which are constants of the list of telemetry names (OBS-05).
    pub const fn new(
        name: &'static str,
        unit: &'static str,
        description: &'static str,
        labels: [&'static str; N],
    ) -> Self {
        Self {
            declared: Declared {
                name,
                unit,
                description,
                labels,
            },
            instrument: OnceLock::new(),
        }
    }

    /// Adds `value`, with one value for each declared label, in order.
    pub fn add(&self, value: u64, labels: [LabelValue; N]) {
        let declared = &self.declared;
        self.instrument
            .get_or_init(|| {
                meter()
                    .u64_counter(declared.name)
                    .with_unit(declared.unit)
                    .with_description(declared.description)
                    .build()
            })
            .add(value, &declared.attributes(labels));
    }
}

/// A distribution of measurements, such as request durations, counted in buckets.
#[derive(Debug)]
pub struct Histogram<const N: usize> {
    declared: Declared<N>,
    boundaries: &'static [f64],
    instrument: OnceLock<opentelemetry::metrics::Histogram<f64>>,
}

impl<const N: usize> Histogram<N> {
    /// A histogram named `name`, measuring in `unit`, such as `s`, whose recordings carry the
    /// labels `labels`, with buckets that end at `boundaries`, in increasing order.
    pub const fn new(
        name: &'static str,
        unit: &'static str,
        description: &'static str,
        labels: [&'static str; N],
        boundaries: &'static [f64],
    ) -> Self {
        Self {
            declared: Declared {
                name,
                unit,
                description,
                labels,
            },
            boundaries,
            instrument: OnceLock::new(),
        }
    }

    /// Counts `value` in its bucket, with one value for each declared label, in order.
    pub fn observe(&self, value: f64, labels: [LabelValue; N]) {
        let declared = &self.declared;
        let instrument = self.instrument.get_or_init(|| {
            meter()
                .f64_histogram(declared.name)
                .with_unit(declared.unit)
                .with_description(declared.description)
                .with_boundaries(self.boundaries.to_vec())
                .build()
        });
        // In path form, so check-instrument doesn't take it for a span's `record`.
        opentelemetry::metrics::Histogram::record(instrument, value, &declared.attributes(labels));
    }
}

/// A value that is set rather than added to, such as whether the database answers. Each
/// combination of label values keeps the last value set for it.
#[derive(Debug)]
pub struct Gauge<const N: usize> {
    declared: Declared<N>,
    instrument: OnceLock<opentelemetry::metrics::Gauge<u64>>,
}

impl<const N: usize> Gauge<N> {
    /// A gauge named `name`, in `unit`, whose recordings carry the labels `labels`.
    pub const fn new(
        name: &'static str,
        unit: &'static str,
        description: &'static str,
        labels: [&'static str; N],
    ) -> Self {
        Self {
            declared: Declared {
                name,
                unit,
                description,
                labels,
            },
            instrument: OnceLock::new(),
        }
    }

    /// Sets the value to `value`, with one value for each declared label, in order.
    pub fn set(&self, value: u64, labels: [LabelValue; N]) {
        let declared = &self.declared;
        let instrument = self.instrument.get_or_init(|| {
            meter()
                .u64_gauge(declared.name)
                .with_unit(declared.unit)
                .with_description(declared.description)
                .build()
        });
        // In path form, so check-instrument doesn't take it for a span's `record`.
        opentelemetry::metrics::Gauge::record(instrument, value, &declared.attributes(labels));
    }
}

/// What every declaration states.
#[derive(Debug)]
struct Declared<const N: usize> {
    name: &'static str,
    unit: &'static str,
    description: &'static str,
    labels: [&'static str; N],
}

impl<const N: usize> Declared<N> {
    /// The data point's attributes: each label with a value, under its declared name.
    fn attributes(&self, values: [LabelValue; N]) -> Vec<KeyValue> {
        self.labels
            .iter()
            .zip(values)
            .filter_map(|(name, LabelValue(value))| value.map(|value| KeyValue::new(*name, value)))
            .collect()
    }
}

/// The meter every declared metric comes from.
#[expect(
    clippy::disallowed_methods,
    reason = "metrics are created only through the declaration types (OBS-11)"
)]
fn meter() -> Meter {
    opentelemetry::global::meter("platform")
}
