//! Health probes (OPS-05) and the startup-failure format (OPS-03).

use std::error::Error;

use axum::{Json, Router, routing::get};
use serde::Serialize;

use crate::errors;

/// The body of every health probe: `{"status": "<status>"}`.
#[derive(Debug, Serialize)]
struct ProbeStatus {
    status: &'static str,
}

/// The health probe routes. They are mounted outside `/v1`, before the middleware stack, so
/// rate limits, load shedding and timeouts never apply to them.
pub fn probe_routes() -> Router {
    Router::new().route("/health/live", get(live))
}

async fn live() -> Json<ProbeStatus> {
    Json(ProbeStatus { status: "alive" })
}

/// Logs a startup failure through the logging setup: one ERROR event with the message
/// `startup failed` and the fields `area` and `error`, where `error` lists the cause chain.
pub fn report_startup_failure(area: &str, error: &(dyn Error + 'static)) {
    tracing::error!(area, error = %errors::chain(error), "startup failed");
}

/// A startup failure as one JSON line with the same message and fields, for a failure before
/// logging is set up.
pub fn startup_failure_line(area: &str, error: &(dyn Error + 'static)) -> String {
    serde_json::json!({
        "level": "ERROR",
        "message": "startup failed",
        "area": area,
        "error": errors::chain(error),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    use serde_json::Value;
    use tracing_subscriber::{filter::LevelFilter, fmt::MakeWriter};

    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("can't bind 0.0.0.0:8080")]
    struct BindFailed(#[source] io::Error);

    /// Collects everything the logger writes.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl MakeWriter<'_> for Captured {
        type Writer = Self;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    fn fields(line: &str) -> [Value; 4] {
        let value: Value = serde_json::from_str(line).unwrap();
        [
            value["level"].clone(),
            value["message"].clone(),
            value["area"].clone(),
            value["error"].clone(),
        ]
    }

    #[test]
    fn ops_03_a_startup_failure_before_logging_is_one_json_line() {
        let error = BindFailed(io::Error::from(io::ErrorKind::AddrInUse));
        let line = startup_failure_line("listen", &error);
        assert!(!line.contains('\n'));
        let [level, message, area, chain] = fields(&line);
        assert_eq!(level, "ERROR");
        assert_eq!(message, "startup failed");
        assert_eq!(area, "listen");
        assert_eq!(
            chain,
            format!(
                "can't bind 0.0.0.0:8080: {}",
                io::Error::from(io::ErrorKind::AddrInUse)
            )
        );
    }

    #[test]
    fn ops_03_a_logged_startup_failure_has_the_same_fields_as_the_line_before_logging() {
        let error = BindFailed(io::Error::from(io::ErrorKind::AddrInUse));
        let captured = Captured::default();
        let subscriber = crate::telemetry::json(LevelFilter::INFO, captured.clone());
        tracing::subscriber::with_default(subscriber, || {
            report_startup_failure("listen", &error);
        });
        let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            fields(lines[0]),
            fields(&startup_failure_line("listen", &error))
        );
    }
}
