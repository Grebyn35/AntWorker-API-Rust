//! Health probes (OPS-05) and the startup-failure format (OPS-03).

use axum::{Json, Router, routing::get};
use serde::Serialize;

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

/// A startup failure as one JSON line, for the case where logging isn't set up yet.
pub fn startup_failure_line(area: &str, error: &dyn std::fmt::Display) -> String {
    serde_json::json!({
        "level": "ERROR",
        "message": "startup failed",
        "area": area,
        "error": error.to_string(),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_03_a_startup_failure_is_one_json_line_with_area_and_error() {
        let line = startup_failure_line("config", &"LISTEN_ADDR is required");
        assert!(!line.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["level"], "ERROR");
        assert_eq!(value["message"], "startup failed");
        assert_eq!(value["area"], "config");
        assert_eq!(value["error"], "LISTEN_ADDR is required");
    }
}
