//! The request pipeline, through the API's routes and a few fixture routes registered the way
//! a module registers its own.

use std::sync::Arc;

use http::{Method, StatusCode};
use platform::telemetry::testing;
use serde_json::{Value, json};

use crate::support::{
    Api,
    api::Response,
    fixtures::{self, Gate},
};

/// A valid inbound trace context, sampled.
const TRACEPARENT: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

/// The trace ID, span ID and flags in a response's `server-timing`.
fn trace_context(response: &Response) -> (String, String, String) {
    let value = response.header("server-timing");
    assert!(
        value.starts_with("trace;desc="),
        "not a trace entry: {value}"
    );
    let traceparent = &value["trace;desc=".len()..];
    let parts: Vec<&str> = traceparent.split('-').collect();
    assert_eq!(parts.len(), 4, "{traceparent}");
    assert_eq!(parts[0], "00");
    (
        parts[1].to_owned(),
        parts[2].to_owned(),
        parts[3].to_owned(),
    )
}

/// Asserts that the response is the problem `code` with `status`, and returns it.
fn problem(response: &Response, status: StatusCode, code: &str) -> Value {
    assert_eq!(response.status, status);
    assert_eq!(response.header("content-type"), "application/problem+json");
    let problem = response.json();
    assert_eq!(problem["type"], format!("/problems/{code}"));
    assert_eq!(problem["status"], status.as_u16());
    assert_eq!(problem["code"], code);
    assert_eq!(problem["trace_id"], trace_context(response).0);
    problem
}

#[tokio::test]
async fn platform_021_a_path_that_does_not_exist_answers_a_not_found_problem() {
    let api = Api::new();

    let response = api.get("/v1/nothing-here").send().await;

    let not_found = problem(&response, StatusCode::NOT_FOUND, "request.not_found");
    assert_eq!(not_found["title"], "Not found");
    assert!(not_found.get("detail").is_none());
}

#[tokio::test]
async fn platform_021_an_internal_error_answers_the_fixed_problem_and_nothing_else() {
    let api = Api::with(fixtures::routes(Arc::default()), 100);

    let response = api.get("/v1/broken").send().await;

    let internal = problem(
        &response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "request.internal_error",
    );
    assert_eq!(internal["title"], "Internal error");
    let members: Vec<&String> = internal.as_object().unwrap().keys().collect();
    assert_eq!(members, ["code", "status", "title", "trace_id", "type"]);
    assert!(!String::from_utf8_lossy(&response.body).contains("disk"));
}

#[tokio::test]
async fn platform_022_every_response_carries_its_trace_context() {
    let api = Api::new();

    for path in ["/v1/capabilities", "/v1/nothing-here"] {
        let response = api.get(path).send().await;

        let (trace, span, flags) = trace_context(&response);
        assert_eq!(trace.len(), 32, "{path}");
        assert_ne!(trace, "0".repeat(32), "{path}");
        assert_eq!(span.len(), 16, "{path}");
        assert_eq!(flags, "01", "every request is sampled (OBS-08)");
    }
}

#[tokio::test]
async fn platform_022_a_valid_traceparent_continues_its_trace() {
    let api = Api::new();

    let response = api
        .get("/v1/capabilities")
        .header("traceparent", TRACEPARENT)
        .send()
        .await;

    let (trace, span, _) = trace_context(&response);
    assert_eq!(trace, "0af7651916cd43dd8448eb211c80319c");
    assert_ne!(
        span, "b7ad6b7169203331",
        "the server's span is a child of the caller's"
    );
}

#[tokio::test]
async fn platform_022_an_unsampled_traceparent_is_still_sampled() {
    let api = Api::new();
    let unsampled = TRACEPARENT.replace("-01", "-00");

    let response = api
        .get("/v1/capabilities")
        .header("traceparent", &unsampled)
        .send()
        .await;

    let (trace, _, flags) = trace_context(&response);
    assert_eq!(trace, "0af7651916cd43dd8448eb211c80319c");
    assert_eq!(flags, "01");
}

#[tokio::test]
async fn platform_022_an_invalid_traceparent_starts_a_new_trace() {
    let api = Api::new();

    let response = api
        .get("/v1/capabilities")
        .header("traceparent", "00-not-a-trace-01")
        .send()
        .await;

    let (trace, _, _) = trace_context(&response);
    assert_ne!(trace, "0af7651916cd43dd8448eb211c80319c");
    assert_ne!(trace, "0".repeat(32));
}

#[tokio::test]
async fn platform_032_a_method_the_path_does_not_serve_answers_405_with_allow() {
    let api = Api::new();

    let response = api.request(Method::POST, "/v1/capabilities").send().await;

    problem(
        &response,
        StatusCode::METHOD_NOT_ALLOWED,
        "request.method_not_allowed",
    );
    assert_eq!(response.header("allow"), "GET,HEAD");
}

#[tokio::test]
async fn platform_035_an_undocumented_query_parameter_answers_400() {
    let api = Api::with(fixtures::routes(Arc::default()), 100);

    let response = api.get("/v1/capabilities?verbose=true").send().await;

    let unknown = problem(&response, StatusCode::BAD_REQUEST, "request.invalid_query");
    assert_eq!(
        unknown["detail"],
        "The route takes no query parameter named verbose."
    );
    let response = api.get("/v1/items?limit=5&sort=name&page=2").send().await;
    let unknown = problem(&response, StatusCode::BAD_REQUEST, "request.invalid_query");
    assert_eq!(
        unknown["detail"],
        "The route takes no query parameter named page, sort."
    );
}

#[tokio::test]
async fn platform_035_a_documented_query_parameter_passes() {
    let api = Api::with(fixtures::routes(Arc::default()), 100);

    let response = api.get("/v1/items?limit=5").send().await;

    assert_eq!(response.status, StatusCode::OK);
}

#[tokio::test]
async fn platform_036_a_body_over_the_limit_answers_413_before_it_is_read() {
    let api = Api::new();
    let limit = 64 * 1024;

    let over = api
        .get("/v1/capabilities")
        .header("content-length", &(limit + 1).to_string())
        .body(vec![b'x'; limit + 1])
        .send()
        .await;
    let at = api
        .get("/v1/capabilities")
        .header("content-length", &limit.to_string())
        .body(vec![b'x'; limit])
        .send()
        .await;

    problem(
        &over,
        StatusCode::PAYLOAD_TOO_LARGE,
        "request.payload_too_large",
    );
    assert_eq!(at.status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn platform_031_a_request_past_its_timeout_answers_503() {
    let api = Api::with(fixtures::routes(Arc::default()), 100);

    let response = api.get("/v1/forever").send().await;

    problem(
        &response,
        StatusCode::SERVICE_UNAVAILABLE,
        "request.timed_out",
    );
}

#[tokio::test]
async fn platform_020_a_request_beyond_the_limit_answers_503_with_retry_after() {
    let gate = Arc::new(Gate::default());
    let api = Api::with(fixtures::routes(gate.clone()), 1);
    let entered = gate.entered.notified();
    let held = tokio::spawn({
        let api = api.clone();
        async move { api.get("/v1/gate").send().await.status }
    });
    entered.await;

    let refused = api.get("/v1/capabilities").send().await;

    problem(
        &refused,
        StatusCode::SERVICE_UNAVAILABLE,
        "request.overloaded",
    );
    assert_eq!(refused.header("retry-after"), "1");
    gate.release.notify_one();
    assert_eq!(held.await.unwrap(), StatusCode::NO_CONTENT);
    let served = api.get("/v1/capabilities").send().await;
    assert_eq!(served.status, StatusCode::OK);
}

#[tokio::test]
async fn platform_019_the_probes_bypass_load_shedding_and_answer_a_plain_status() {
    let gate = Arc::new(Gate::default());
    let api = Api::with(fixtures::routes(gate.clone()), 1);
    let entered = gate.entered.notified();
    let held = tokio::spawn({
        let api = api.clone();
        async move { api.get("/v1/gate").send().await.status }
    });
    entered.await;

    let live = api.get("/health/live").send().await;
    let ready = api.get("/health/ready").send().await;

    assert_eq!(live.status, StatusCode::OK);
    assert_eq!(live.json(), json!({"status": "alive"}));
    assert_eq!(ready.status, StatusCode::OK);
    assert_eq!(ready.json(), json!({"status": "ready"}));
    for probe in [&live, &ready] {
        assert!(probe.headers.get("server-timing").is_none(), "OBS-15");
    }
    gate.release.notify_one();
    held.await.unwrap();
}

#[tokio::test]
async fn err_12_a_panic_answers_the_internal_problem() {
    let api = Api::with(fixtures::routes(Arc::default()), 100);

    let response = api.get("/v1/panic").send().await;

    let internal = problem(
        &response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "request.internal_error",
    );
    assert!(internal.get("detail").is_none());
    assert!(!String::from_utf8_lossy(&response.body).contains("fixture"));
}

#[tokio::test]
async fn sec_11_every_response_carries_the_security_headers() {
    let api = Api::new();

    for path in [
        "/v1/capabilities",
        "/v1/nothing-here",
        "/health/live",
        "/health/ready",
    ] {
        let response = api.get(path).send().await;

        assert_eq!(
            response.header("x-content-type-options"),
            "nosniff",
            "{path}"
        );
        assert_eq!(response.header("referrer-policy"), "no-referrer", "{path}");
    }
}

#[tokio::test]
async fn platform_034_a_request_writes_one_completion_line() {
    let api = Api::new();

    let response = api.get("/v1/capabilities").send().await;

    let (trace, _, _) = trace_context(&response);
    let lines = testing::logged(&trace);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    let line = &lines[0];
    assert_eq!(line["level"], "INFO");
    assert_eq!(line["message"], "request completed");
    assert_eq!(line["http.request.method"], "GET");
    assert_eq!(line["http.route"], "/v1/capabilities");
    assert_eq!(line["http.response.status_code"], 200);
    assert!(line["request.duration"].as_f64().unwrap() >= 0.0);
}

#[tokio::test]
async fn platform_034_a_4xx_writes_nothing_above_info() {
    let api = Api::new();

    let response = api.get("/v1/nothing-here").send().await;

    let (trace, _, _) = trace_context(&response);
    let lines = testing::logged(&trace);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(lines[0]["level"], "INFO");
    assert_eq!(lines[0]["http.response.status_code"], 404);
    assert!(lines[0].get("http.route").is_none(), "no route matched");
}

#[tokio::test]
async fn platform_034_a_5xx_writes_one_error_line_with_its_cause_chain() {
    let api = Api::with(fixtures::routes(Arc::default()), 100);

    let response = api.get("/v1/broken").send().await;

    let (trace, _, _) = trace_context(&response);
    let lines = testing::logged(&trace);
    let levels: Vec<&Value> = lines.iter().map(|line| &line["level"]).collect();
    assert_eq!(levels, ["ERROR", "INFO"], "{lines:#?}");
    assert_eq!(lines[0]["message"], "request failed");
    assert_eq!(lines[0]["error.type"], "request.internal_error");
    assert_eq!(
        lines[0]["exception.message"],
        "internal error: the disk is gone"
    );
    assert_eq!(lines[1]["http.response.status_code"], 500);
}
