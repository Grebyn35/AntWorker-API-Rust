//! The instance's own memory, which `serve` records every 15 seconds in `process.memory.usage`
//! (PLATFORM-046), so a dashboard shows a replica whose memory only grows. The resident set comes
//! from the kernel's report of the process, `/proc/self/status`, so it is recorded on Linux only.

use std::time::Duration;

use opentelemetry_semantic_conventions::metric as otel;

use crate::{
    clock::SharedClock,
    telemetry::{metrics::Gauge, names::convention},
};

/// How often the memory is recorded, as often as the metrics are exported.
const INTERVAL: Duration = Duration::from_secs(15);

static MEMORY_USAGE: Gauge<0> = Gauge::new(
    convention("process.memory.usage", otel::PROCESS_MEMORY_USAGE),
    "By",
    "The physical memory the instance's process uses: its resident set.",
    [],
);

/// Records the process's resident memory now and every interval after, for as long as the runtime
/// runs.
pub fn watch(clock: SharedClock) {
    tokio::spawn(async move {
        loop {
            if let Some(bytes) = resident().await {
                MEMORY_USAGE.set(bytes, []);
            }
            clock.sleep(INTERVAL).await;
        }
    });
}

/// The process's resident set, in bytes, or none where the kernel doesn't report it.
async fn resident() -> Option<u64> {
    let status = tokio::fs::read_to_string("/proc/self/status").await.ok()?;
    resident_in(&status)
}

/// The resident set that `status`, the text of `/proc/<pid>/status`, reports, in bytes.
fn resident_in(status: &str) -> Option<u64> {
    let kilobytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse::<u64>()
        .ok()?;
    kilobytes.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_046_the_resident_set_is_read_from_the_kernel_s_report() {
        let status =
            "Name:\tantworker-api\nVmPeak:\t  30000 kB\nVmRSS:\t   12345 kB\nThreads:\t8\n";

        assert_eq!(resident_in(status), Some(12_345 * 1024));
        assert_eq!(resident_in("Name:\tantworker-api\n"), None);
    }
}
