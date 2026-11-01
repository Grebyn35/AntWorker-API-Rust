//! The OpenAPI document (API-17).

use std::collections::BTreeSet;

use http::{Method, StatusCode};

use crate::support::{Api, run_binary};

/// The document as committed at the repository's root.
const COMMITTED: &str = include_str!("../../../../openapi.json");

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test may block (CODE-05), and reads the file the binary wrote"
)]
async fn api_17_the_committed_openapi_json_is_the_one_the_binary_writes() {
    let path = std::env::temp_dir().join(format!("openapi-{}.json", std::process::id()));

    let output = run_binary(&["openapi", path.to_str().unwrap()], &[]).await;

    assert!(output.status.success(), "{output:?}");
    let written = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(
        written == COMMITTED.replace("\r\n", "\n"),
        "openapi.json is out of date; run `just openapi`"
    );
}

#[test]
fn err_13_every_code_the_pipeline_answers_is_in_the_committed_document() {
    let document: serde_json::Value = serde_json::from_str(COMMITTED).unwrap();
    let text = document.to_string();

    for error in platform::errors::RequestError::every() {
        let code = platform::errors::ErrorCode::code(&error);
        assert!(
            text.contains(&format!("\"{code}\"")),
            "{code} isn't in openapi.json"
        );
        assert!(
            text.contains(&format!(
                "`{code}`: {}.",
                platform::errors::ErrorCode::title(&error)
            )),
            "openapi.json doesn't say what {code} means"
        );
    }
}

#[tokio::test]
async fn api_14_every_user_route_refuses_a_caller_without_a_credential() {
    let api = Api::new().await;
    let paths = api.document()["paths"].as_object().unwrap().clone();
    let mut called = 0;

    for (template, item) in &paths {
        // An ID that parses, so the request reaches authentication.
        let path: String = template
            .split('/')
            .map(|part| {
                if part.starts_with('{') {
                    "01900000-0000-7000-8000-000000000000"
                } else {
                    part
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        for (method, operation) in item.as_object().unwrap() {
            if operation["x-access"] != "user" {
                continue;
            }
            let method = Method::from_bytes(method.to_ascii_uppercase().as_bytes()).unwrap();
            let response = api
                .request(method.clone(), &path)
                .header("content-type", "application/json")
                .body("{}")
                .send()
                .await;

            assert_eq!(
                response.status,
                StatusCode::UNAUTHORIZED,
                "{method} {template}"
            );
            assert_eq!(response.json()["code"], "request.unauthenticated");
            called += 1;
        }
    }
    assert!(called >= 3, "only {called} user routes");
}

#[tokio::test]
async fn api_17_every_path_serves_exactly_the_methods_its_documentation_lists() {
    let api = Api::new().await;
    let methods = [
        "get", "put", "post", "delete", "options", "head", "patch", "trace",
    ];
    let paths = api.document()["paths"].as_object().unwrap().clone();

    for (template, item) in &paths {
        // axum answers 405 before any extractor runs, so any value fills a parameter.
        let path: String = template
            .split('/')
            .map(|part| if part.starts_with('{') { "0" } else { part })
            .collect::<Vec<_>>()
            .join("/");
        let mut documented: BTreeSet<String> = item
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| methods.contains(&key.as_str()))
            .cloned()
            .collect();
        if documented.contains("get") {
            documented.insert("head".to_owned());
        }
        // Each method OpenAPI can document that the path doesn't, since a method router's
        // fallback could serve one and leave the others to the 405, and one no route serves.
        let undocumented = methods
            .iter()
            .filter(|method| !documented.contains(**method))
            .map(|method| method.to_ascii_uppercase())
            .chain(["PROBE".to_owned()]);

        for method in undocumented {
            let response = api
                .request(Method::from_bytes(method.as_bytes()).unwrap(), &path)
                .send()
                .await;

            assert_eq!(
                response.status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {template}"
            );
            let served: BTreeSet<String> = response
                .header("allow")
                .split(',')
                .map(|allowed| allowed.trim().to_ascii_lowercase())
                .collect();
            assert_eq!(
                served, documented,
                "{template} serves other methods than it documents"
            );
        }
    }
}

#[tokio::test]
#[should_panic(expected = "POST /v1/users was sent a body that breaks its documentation")]
async fn api_17_a_typed_request_whose_body_breaks_the_document_fails() {
    let api = Api::new().await;

    api.post(
        "/v1/users",
        &serde_json::json!({ "email": "not-an-address", "colour": "red" }),
    )
    .send()
    .await;
}
