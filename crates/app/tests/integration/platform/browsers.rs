//! CORS and the cross-site check (PLATFORM-058, PLATFORM-059).

use std::sync::Arc;

use http::{Method, StatusCode};
use serde_json::json;

use crate::support::{
    Api, Options,
    fixtures::{self, Gate},
    users::{self, WEB, browser_sign_in, browsers_api, cookies},
};

#[tokio::test]
async fn platform_058_a_listed_origin_may_read_responses_and_send_its_cookies() {
    let api = browsers_api().await;

    let response = api
        .get("/v1/capabilities")
        .header("origin", WEB)
        .send()
        .await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("access-control-allow-origin"), WEB);
    assert_eq!(response.header("access-control-allow-credentials"), "true");
    assert!(
        response
            .header("access-control-expose-headers")
            .contains("ratelimit")
    );
    assert!(response.headers("vary").contains(&"origin"));
}

#[tokio::test]
async fn platform_058_a_listed_origin_s_preflight_is_answered() {
    let api = browsers_api().await;

    let response = api
        .request(Method::OPTIONS, "/v1/users")
        .header("origin", WEB)
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type")
        .send()
        .await;

    assert_eq!(response.status, 204);
    assert_eq!(response.header("access-control-allow-origin"), WEB);
    assert!(
        response
            .header("access-control-allow-methods")
            .contains("POST")
    );
    assert!(
        response
            .header("access-control-allow-headers")
            .contains("content-type")
    );
}

#[tokio::test]
async fn platform_058_any_other_origin_gets_no_cors_header() {
    let api = browsers_api().await;

    let other = api
        .get("/v1/capabilities")
        .header("origin", "https://evil.example.com")
        .send()
        .await;
    let preflight = api
        .request(Method::OPTIONS, "/v1/users")
        .header("origin", "https://evil.example.com")
        .header("access-control-request-method", "POST")
        .send()
        .await;

    assert!(other.headers.get("access-control-allow-origin").is_none());
    assert!(
        preflight
            .headers
            .get("access-control-allow-origin")
            .is_none()
    );
    assert_eq!(preflight.status, 405);
}

#[tokio::test]
async fn platform_058_a_listed_origin_s_preflight_is_answered_before_the_ip_limit() {
    let api = Api::with(
        Options::default()
            .set("CORS_ALLOWED_ORIGINS", WEB)
            .set("RATE_LIMIT_IP_REQUESTS", 1),
    )
    .await;

    for _ in 0..3 {
        let preflight = api
            .request(Method::OPTIONS, "/v1/users")
            .header("origin", WEB)
            .header("access-control-request-method", "POST")
            .send()
            .await;
        assert_eq!(preflight.status, 204);
    }
    let first = api.get("/v1/capabilities").send().await;

    assert_eq!(first.status, 200, "no preflight counted");
}

#[tokio::test]
async fn platform_058_a_listed_origin_may_read_the_ip_limit_s_429() {
    let api = Api::with(
        Options::default()
            .set("CORS_ALLOWED_ORIGINS", WEB)
            .set("RATE_LIMIT_IP_REQUESTS", 1),
    )
    .await;
    let counted = api
        .get("/v1/capabilities")
        .header("origin", WEB)
        .send()
        .await;

    let refused = api
        .get("/v1/capabilities")
        .header("origin", WEB)
        .send()
        .await;

    assert_eq!(counted.status, StatusCode::OK);
    assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(refused.header("access-control-allow-origin"), WEB);
    assert_eq!(refused.header("access-control-allow-credentials"), "true");
}

#[tokio::test]
async fn platform_058_a_listed_origin_may_read_the_concurrency_limit_s_503() {
    let gate = Arc::new(Gate::default());
    let api = Api::with(
        Options::routes(fixtures::routes(gate.clone()))
            .set("CORS_ALLOWED_ORIGINS", WEB)
            .set("MAX_CONCURRENT_REQUESTS", 1),
    )
    .await;
    let entered = gate.entered.notified();
    let held = tokio::spawn({
        let api = api.clone();
        async move { api.get("/v1/gate").send().await.status }
    });
    entered.await;

    let refused = api
        .get("/v1/capabilities")
        .header("origin", WEB)
        .send()
        .await;

    assert_eq!(refused.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused.header("access-control-allow-origin"), WEB);
    assert_eq!(refused.header("access-control-allow-credentials"), "true");
    gate.release.notify_one();
    assert_eq!(held.await.unwrap(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn platform_059_a_change_from_an_unlisted_origin_answers_403() {
    let api = browsers_api().await;
    for origin in ["https://evil.example.com", "null"] {
        let response = api
            .post("/v1/users", &users::registration("ada@example.com"))
            .header("origin", origin)
            .send()
            .await;

        assert_eq!(response.status, 403, "{origin}");
        assert_eq!(response.json()["code"], "request.origin_not_allowed");
    }
    let without_origin = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    assert_eq!(
        without_origin.status, 201,
        "a client that sets its own headers"
    );
}

#[tokio::test]
async fn platform_059_a_refusal_of_an_origin_counts_in_the_ip_limit_and_carries_its_headers() {
    let api = Api::with(
        Options::default()
            .set("CORS_ALLOWED_ORIGINS", WEB)
            .set("RATE_LIMIT_IP_REQUESTS", 2),
    )
    .await;
    let mut answers = Vec::new();

    for _ in 0..3 {
        let response = api
            .post("/v1/users", &users::registration("ada@example.com"))
            .header("origin", "https://evil.example.com")
            .send()
            .await;
        answers.push(response);
    }

    for (answer, remaining) in answers[..2].iter().zip([1, 0]) {
        assert_eq!(answer.status, StatusCode::FORBIDDEN);
        assert_eq!(answer.json()["code"], "request.origin_not_allowed");
        assert_eq!(answer.header("ratelimit-policy"), "\"ip\";q=2;w=60");
        let state = format!("\"ip\";r={remaining};t=");
        assert!(answer.header("ratelimit").starts_with(&state), "{state}");
    }
    // The two refusals counted, so the third request is over the limit.
    assert_eq!(answers[2].status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(answers[2].json()["code"], "request.rate_limited");
}

#[tokio::test]
async fn platform_059_a_change_a_cookie_authenticates_needs_a_listed_origin() {
    let api = browsers_api().await;
    let cookie = cookies(&browser_sign_in(&api).await);

    let without = api
        .request(Method::DELETE, "/v1/users/me/sessions/current")
        .header("cookie", &cookie)
        .send()
        .await;
    let refresh = api
        .post("/v1/sessions/refresh", &json!({}))
        .header("cookie", &cookie)
        .send()
        .await;
    let listed = api
        .request(Method::DELETE, "/v1/users/me/sessions/current")
        .header("cookie", &cookie)
        .header("origin", WEB)
        .send()
        .await;

    assert_eq!(without.status, 403);
    assert_eq!(without.json()["code"], "request.origin_not_allowed");
    assert_eq!(refresh.status, 403);
    assert_eq!(listed.status, 204);
}
