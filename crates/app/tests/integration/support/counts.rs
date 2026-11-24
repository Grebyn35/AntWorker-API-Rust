//! What a process of a test's own counted, which no other test's requests reach (TEST-07): the
//! counters and the histograms that a `serve` exported to the stand-in collector, read once it
//! has stopped (`Server::stop`), since it writes out its metrics as it exits (PLATFORM-068).

use std::collections::BTreeMap;

use opentelemetry_proto::tonic::metrics::v1::{NumberDataPoint, metric, number_data_point};
use platform::telemetry::testing::collector::{Received, attribute, text};

/// The counter `name` in the last export of `received` that holds it, as the value of its
/// attribute `label` and the count, such as `stored 1`, for each value counted, in the values'
/// order: none when no export holds it.
pub fn of(received: &Received, name: &str, label: &str) -> Vec<String> {
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for point in points(received, name) {
        let counted = attribute(&point.attributes, label).unwrap_or_default();
        *counts.entry(counted).or_default() += value(&point);
    }
    counts
        .into_iter()
        .map(|(counted, value)| format!("{counted} {value}"))
        .collect()
}

/// The histogram `http.server.request.duration` in the last export of `received` that holds it,
/// as each data point's attributes, each `key=value` in the keys' order, then how many recordings
/// it holds, such as `http.request.method=GET http.response.status_code=404 1`, in the order of
/// those texts: none when no export holds it.
pub fn recordings(received: &Received) -> Vec<String> {
    let points = received
        .metrics()
        .into_iter()
        .find(|(_, metric)| metric.name == "http.server.request.duration")
        .and_then(|(_, metric)| match &metric.data {
            Some(metric::Data::Histogram(histogram)) => Some(histogram.data_points.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let mut recorded: Vec<String> = points
        .iter()
        .map(|point| {
            let labels: BTreeMap<&str, String> = point
                .attributes
                .iter()
                .map(|pair| {
                    let value = pair.value.as_ref().and_then(text).unwrap_or_default();
                    (pair.key.as_str(), value)
                })
                .collect();
            let labels: Vec<String> = labels
                .into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            format!("{} {}", labels.join(" "), point.count)
        })
        .collect();
    recorded.sort();
    recorded
}

/// The data points of the counter `name` in the last export of `received` that holds it.
fn points(received: &Received, name: &str) -> Vec<NumberDataPoint> {
    received
        .metrics()
        .into_iter()
        .find(|(_, metric)| metric.name == name)
        .and_then(|(_, metric)| match &metric.data {
            Some(metric::Data::Sum(sum)) => Some(sum.data_points.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The count of a counter's data point, which the API's counters export as a whole number.
fn value(point: &NumberDataPoint) -> i64 {
    match point.value {
        Some(number_data_point::Value::AsInt(value)) => Some(value),
        _ => None,
    }
    .expect("a count that is a whole number")
}
