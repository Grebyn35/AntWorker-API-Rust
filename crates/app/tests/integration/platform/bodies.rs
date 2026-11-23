//! Request bodies (PLATFORM-049 to PLATFORM-051).

use http::Method;
use serde_json::json;

use crate::support::{Api, users};

#[tokio::test]
async fn platform_049_a_body_that_isn_t_the_route_s_json_answers_400() {
    let api = Api::new().await;
    let valid = users::registration("ada@example.com").to_string();
    for (content_type, body, field) in [
        ("text/plain", valid.clone(), None),
        ("application/json", "{\"email\":".to_owned(), None),
        ("application/json", format!("{valid}{valid}"), None),
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
async fn platform_050_a_body_that_grows_past_the_limit_answers_413() {
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
async fn platform_051_every_field_its_domain_type_refuses_is_named() {
    let api = Api::new().await;
    let body = json!({ "email": "nope", "password": "short", "name": " ", "device_id": "bad id" });

    let response = api.post_raw("/v1/users", &body).send().await;

    assert_eq!(
        response.refused_fields(),
        [
            ("/email", "invalid_email"),
            ("/password", "too_short"),
            ("/name", "required"),
            ("/device_id", "invalid_device_id"),
        ]
        .map(|(field, code)| (field.to_owned(), code.to_owned()))
    );
}

#[tokio::test]
async fn platform_051_every_field_a_sign_in_refuses_is_named() {
    let api = Api::new().await;
    let body = json!({ "email": "nope", "password": "", "device_id": "bad id" });

    let response = api.post_raw("/v1/sessions", &body).send().await;

    assert_eq!(
        response.refused_fields(),
        [
            ("/email", "invalid_email"),
            ("/password", "required"),
            ("/device_id", "invalid_device_id"),
        ]
        .map(|(field, code)| (field.to_owned(), code.to_owned()))
    );
}

#[tokio::test]
async fn platform_051_every_field_a_re_authentication_refuses_is_named() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let path = "/v1/users/me/sessions/current/reauthenticate";

    let empty = api
        .post_raw(path, &json!({ "password": "", "code": "12345" }))
        .bearer(&ada.access)
        .send()
        .await;
    let long = api
        .post_raw(
            path,
            &json!({ "password": "p".repeat(129), "recovery_code": "ABCD-EFGH" }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    for (response, password, code_at) in [
        (&empty, "required", "/code"),
        (&long, "too_long", "/recovery_code"),
    ] {
        assert_eq!(
            response.refused_fields(),
            [("/password", password), (code_at, "invalid_code")]
                .map(|(field, code)| (field.to_owned(), code.to_owned()))
        );
    }
}
