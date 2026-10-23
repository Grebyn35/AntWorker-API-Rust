//! CPU-heavy work, such as hashing a password: it runs on a blocking thread, so it never stalls
//! the async runtime (CODE-05), inside a span of its own, so it shows in traces (OBS-06).

use tokio::task::JoinError;

use crate::telemetry::names;

/// Runs `work` on a blocking thread, inside a span named `name` that is a child of the current
/// span. Fails only when `work` panics.
pub async fn run<T>(
    name: &'static str,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, JoinError>
where
    T: Send + 'static,
{
    let span = tracing::info_span!("blocking", { names::OTEL_NAME } = name);
    tokio::task::spawn_blocking(move || span.in_scope(work)).await
}
