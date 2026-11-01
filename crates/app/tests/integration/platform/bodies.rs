//! Request bodies (PLATFORM-054 to PLATFORM-056).

use http::Method;
use serde_json::json;

use crate::support::{Api, users};

#[tokio::test]
async fn platform_054_a_body_that_isn_t_the_route_s_json_answers_400() {
    let api = Api::new().await;
    let valid = users::registration("ada@example.com").to_string();
    for (content_type, body, field) in [
        ("text/plain", valid.clone(), None),
        ("application/json", "{\"email\":".to_owned(), None),
        (
            "application/json",
            json!({ "email": "ada@example.com", "password": users::PASSWORD, "name": "Ada", "role": "admin" })
                .to_string(),
            Some("/role"),
        ),
        (
            "application/json",
            json!({ "email": 7, "password": users::PASSWORD, "name": "Ada" }).to_string(),
            Some("/email"),
        ),
    ] {
        let response = api
            .request(Method::POST, "/v1/users")
            .header("content-type", content_type)
            .body(body)
            .send()
            .await;

        assert_eq!(response.status, 400);
        let problem = response.json();
        assert_eq!(problem["code"], "request.invalid_body");
        assert_eq!(problem["errors"][0]["field"].as_str(), field);
    }
}

#[tokio::test]
async fn platform_055_a_body_that_grows_past_the_limit_answers_413() {
    let api = Api::new().await;
    let name = "a".repeat(70 * 1024);
    let body = json!({ "email": "ada@example.com", "password": users::PASSWORD, "name": name });

    let response = api
        .request(Method::POST, "/v1/users")
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await;

    assert_eq!(response.status, 413);
    assert_eq!(response.json()["code"], "request.payload_too_large");
}

#[tokio::test]
async fn platform_056_every_field_its_domain_type_refuses_is_named() {
    let api = Api::new().await;
    let body = json!({ "email": "nope", "password": "short", "name": " ", "device_id": "bad id" });

    let response = api.post("/v1/users", &body).send().await;

    assert_eq!(response.status, 400);
    let problem = response.json();
    assert_eq!(problem["code"], "request.invalid_fields");
    let named: Vec<(&str, &str)> = problem["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| {
            (
                error["field"].as_str().unwrap(),
                error["code"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            ("/email", "invalid_email"),
            ("/password", "too_short"),
            ("/name", "required"),
            ("/device_id", "invalid_device_id"),
        ]
    );
}
