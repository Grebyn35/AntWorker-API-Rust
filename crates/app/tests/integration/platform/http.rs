//! The request pipeline, through the API's routes and a few fixture routes registered the way
//! a module registers its own.

use std::sync::Arc;

use http::{Method, StatusCode};
use platform::telemetry::testing;
use serde_json::{Value, json};

use crate::support::{
    Api, Options,
    api::Response,
    fixtures::{self, Gate},
    users,
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
async fn platform_040_a_path_that_does_not_exist_answers_a_not_found_problem() {
    let api = Api::new().await;

    let response = api.get("/v1/nothing-here").send().await;

    let not_found = problem(&response, StatusCode::NOT_FOUND, "request.not_found");
    assert_eq!(not_found["title"], "Not found");
    assert!(not_found.get("detail").is_none());
}

#[tokio::test]
async fn platform_040_an_internal_error_answers_the_fixed_problem_and_nothing_else() {
    let api = Api::with(Options::routes(fixtures::routes(Arc::default()))).await;

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
async fn platform_041_every_response_carries_its_trace_context() {
    let api = Api::new().await;

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
async fn platform_041_a_valid_traceparent_continues_its_trace() {
    let api = Api::new().await;

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
async fn platform_041_an_unsampled_traceparent_is_still_sampled() {
    let api = Api::new().await;
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
async fn platform_041_an_invalid_traceparent_starts_a_new_trace() {
    let api = Api::new().await;

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
async fn platform_043_a_method_the_path_does_not_serve_answers_405_with_allow() {
    let api = Api::new().await;

    let response = api.request(Method::POST, "/v1/capabilities").send().await;

    problem(
        &response,
        StatusCode::METHOD_NOT_ALLOWED,
        "request.method_not_allowed",
    );
    assert_eq!(response.header("allow"), "GET,HEAD");
}

#[tokio::test]
async fn platform_045_an_undocumented_query_parameter_answers_400() {
    let api = Api::new().await;

    let response = api.get("/v1/capabilities?verbose=true").send().await;

    let unknown = problem(&response, StatusCode::BAD_REQUEST, "request.invalid_query");
    let message = "The route takes no query parameter of this name.";
    assert_eq!(
        unknown["errors"],
        serde_json::json!([{ "field": "/verbose", "code": "unknown", "message": message }])
    );
    let ada = users::register(&api, "ada@example.com").await;
    let response = api
        .get("/v1/workspaces?limit=5&sort=name&page=2")
        .bearer(&ada.access)
        .send()
        .await;
    let unknown = problem(&response, StatusCode::BAD_REQUEST, "request.invalid_query");
    assert_eq!(
        unknown["errors"],
        serde_json::json!([
            { "field": "/page", "code": "unknown", "message": message },
            { "field": "/sort", "code": "unknown", "message": message },
        ])
    );
}

#[tokio::test]
async fn platform_045_a_head_request_is_checked_like_its_get() {
    let api = Api::new().await;

    let response = api
        .request(Method::HEAD, "/v1/capabilities?verbose=true")
        .send()
        .await;

    assert_eq!(response.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn platform_045_a_documented_query_parameter_passes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = api
        .get("/v1/workspaces?limit=5")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, StatusCode::OK);
}

#[tokio::test]
async fn platform_048_a_body_over_the_limit_answers_413_before_it_is_read() {
    let api = Api::new().await;
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

#[tokio::test]
async fn platform_042_a_request_past_its_timeout_answers_503() {
    let gate = Arc::new(Gate::default());
    let api = Api::with(Options::routes(fixtures::routes(gate.clone()))).await;
    let entered = gate.entered.notified();
    let request = tokio::spawn({
        let api = api.clone();
        async move { api.get("/v1/gate").send().await }
    });
    entered.await;
    // Paused once the request waits in its handler, after its rate limit's count in the
    // database, so tokio's time runs straight to the route's timeout, the only timer left.
    tokio::time::pause();

    let response = request.await.unwrap();

    problem(
        &response,
        StatusCode::SERVICE_UNAVAILABLE,
        "request.timed_out",
    );
    assert_eq!(response.header("retry-after"), "1");
}

#[tokio::test]
async fn platform_039_a_request_beyond_the_limit_answers_503_with_retry_after() {
    let gate = Arc::new(Gate::default());
    let api = Api::with(
        Options::routes(fixtures::routes(gate.clone())).set("MAX_CONCURRENT_REQUESTS", 1),
    )
    .await;
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
async fn platform_038_the_probes_bypass_load_shedding_and_answer_a_plain_status() {
    let gate = Arc::new(Gate::default());
    let api = Api::with(
        Options::routes(fixtures::routes(gate.clone())).set("MAX_CONCURRENT_REQUESTS", 1),
    )
    .await;
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
async fn platform_038_the_probes_bypass_the_route_groups_body_limit() {
    let api = Api::new().await;
    let over = 64 * 1024 + 1;

    let live = api
        .get("/health/live")
        .header("content-length", &over.to_string())
        .body(vec![b'x'; over])
        .send()
        .await;

    assert_eq!(live.status, StatusCode::OK);
}

#[tokio::test]
async fn err_12_a_panic_answers_the_internal_problem() {
    let api = Api::with(Options::routes(fixtures::routes(Arc::default()))).await;

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
async fn platform_044_a_panic_leaves_its_error_line_to_the_panic_hook() {
    let api = Api::with(Options::routes(fixtures::routes(Arc::default()))).await;

    let response = api.get("/v1/panic").send().await;

    // The tests' telemetry installs the API's panic hook (ERR-12), whose line comes first; the
    // request log writes only its completion line.
    let (trace, _, _) = trace_context(&response);
    let lines = testing::logged(&trace);
    let levels: Vec<&Value> = lines.iter().map(|line| &line["level"]).collect();
    assert_eq!(levels, ["ERROR", "INFO"], "{lines:#?}");
    assert_eq!(lines[0]["message"], "panicked");
    assert_eq!(lines[1]["http.response.status_code"], 500);
}

#[tokio::test]
async fn sec_12_every_response_carries_the_security_headers() {
    let api = Api::new().await;

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
        assert_eq!(response.header("cache-control"), "no-store", "{path}");
    }
}

#[tokio::test]
async fn platform_044_a_request_writes_one_completion_line() {
    let api = Api::new().await;

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
async fn platform_044_a_4xx_writes_nothing_above_info() {
    let api = Api::new().await;

    let response = api.get("/v1/nothing-here").send().await;

    let (trace, _, _) = trace_context(&response);
    let lines = testing::logged(&trace);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(lines[0]["level"], "INFO");
    assert_eq!(lines[0]["http.response.status_code"], 404);
    assert!(lines[0].get("http.route").is_none(), "no route matched");
}

#[tokio::test]
async fn platform_044_a_5xx_writes_one_error_line_with_its_cause_chain() {
    let api = Api::with(Options::routes(fixtures::routes(Arc::default()))).await;

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

#[tokio::test]
async fn platform_052_development_serves_the_api_reference_of_the_running_code() {
    let api = Api::with(Options::default().set("ENVIRONMENT", "development")).await;

    let page = api.get("/docs").send().await;
    let document = api.get("/docs/openapi.json").send().await;
    // Each is in a route group, as a builder's route is (OPS-07, API-09).
    let queried = api.get("/docs?theme=dark").send().await;

    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(page.header("content-type"), "text/html; charset=utf-8");
    let html = String::from_utf8(page.body.clone()).unwrap();
    for pinned in [
        "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.34.0/swagger-ui-bundle.js\" integrity=\"sha384-",
        "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.34.0/swagger-ui.css\" integrity=\"sha384-",
        "url: \"/docs/openapi.json\"",
        "validatorUrl: null",
    ] {
        assert!(html.contains(pinned), "{pinned}");
    }
    assert!(page.header("server-timing").starts_with("trace;desc="));
    assert_eq!(document.status, StatusCode::OK);
    assert_eq!(document.header("content-type"), "application/json");
    assert_eq!(&document.json(), api.document());
    problem(&queried, StatusCode::BAD_REQUEST, "request.invalid_query");
}

#[tokio::test]
async fn platform_052_any_other_environment_answers_the_reference_s_paths_with_404() {
    let api = Api::new().await;

    for path in ["/docs", "/docs/openapi.json"] {
        let response = api.get(path).send().await;

        problem(&response, StatusCode::NOT_FOUND, "request.not_found");
    }
}

#[tokio::test]
async fn platform_052_the_development_settings_let_the_reference_s_requests_through_the_cross_site_check()
 {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let example = tokio::fs::read_to_string(format!("{root}/.env.example"))
        .await
        .unwrap();
    let setting = |name: &str| {
        example
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .unwrap()
            .to_owned()
    };
    let port = setting("LISTEN_ADDR")
        .rsplit(':')
        .next()
        .unwrap()
        .to_owned();
    let origins = setting("CORS_ALLOWED_ORIGINS");
    let api = Api::with(
        Options::default()
            .set("ENVIRONMENT", "development")
            .set("CORS_ALLOWED_ORIGINS", &origins),
    )
    .await;

    for (n, origin) in [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
    ]
    .into_iter()
    .enumerate()
    {
        let registered = api
            .post(
                "/v1/users",
                &users::registration(&format!("reference-{n}@example.com")),
            )
            .header("origin", &origin)
            .send()
            .await;

        assert!(
            origins.split(',').any(|listed| listed.trim() == origin),
            "{origin} isn't in {origins}"
        );
        assert_eq!(
            registered.status,
            StatusCode::CREATED,
            "{}",
            registered.json()
        );
        assert!(registered.header("set-cookie").starts_with("__Host-"));
    }
}
