//! The OpenAPI document (API-17).

use std::collections::BTreeSet;

use http::{Method, StatusCode};

use crate::support::{Api, run_binary, users, workspaces};

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
async fn api_14_every_route_that_isn_t_public_refuses_a_caller_without_a_credential() {
    let api = Api::new().await;
    let mut called = 0;

    for (method, path, access) in api.operations() {
        if access == "public" {
            continue;
        }
        let response = api
            .request(method.clone(), &path)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await;

        assert_eq!(response.status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(response.json()["code"], "request.unauthenticated");
        called += 1;
    }
    assert!(called >= 7, "only {called} routes that aren't public");
}

#[tokio::test]
async fn api_10_every_parameter_of_a_list_refuses_a_value_that_is_none_by_its_name() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    // part: superadmins
    crate::support::superadmins::make_with_app(&api, &ada).await;
    // end part: superadmins
    let paths = api.document()["paths"].as_object().unwrap().clone();
    let mut sent = 0;

    for (template, item) in &paths {
        let names: Vec<&str> = item["get"]["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|parameter| parameter["in"] == "query")
            .filter_map(|parameter| parameter["name"].as_str())
            .collect();
        // A list is what takes `cursor` (API-06).
        if !names.contains(&"cursor") {
            continue;
        }
        let path = template.replace("{workspace_id}", &workspace);
        for name in names {
            // A control character, which no parameter's type takes.
            let response = api
                .get(&format!("{path}?{name}=%01"))
                .bearer(&ada.access)
                .send()
                .await;

            assert_eq!(
                response.status,
                StatusCode::BAD_REQUEST,
                "{template} {name}"
            );
            let problem = response.json();
            assert_eq!(problem["code"], "request.invalid_query", "{template}");
            let fields: Vec<&str> = problem["errors"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|error| error["field"].as_str())
                .collect();
            assert_eq!(fields, [format!("/{name}")], "{template}");
            sent += 1;
        }
    }
    assert!(sent >= 6, "only {sent} parameters");
}

#[tokio::test]
async fn platform_088_the_pattern_that_documents_a_moment_takes_the_forms_that_a_filter_takes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    // What a filter takes; what it refuses by its form; and what it refuses by its value, a day
    // that no calendar holds or a second past 59, which a pattern of digits can't tell.
    let taken = [
        "2026-11-01T02:26:40Z",
        "2026-11-01T02:26:40.2Z",
        "2026-11-01T02:26:40.123456Z",
    ];
    let malformed = [
        "2026-11-01T02:26:40.1234567Z",
        "2026-11-01T02:26:40.Z",
        "2026-11-01T02:26:40",
        "2026-11-01T02:26:40+00:00",
        "2026-11-01 02:26:40Z",
        "2026-+1-01T02:26:40Z",
        "2026-11-01T02:26:40.12a4Z",
        "2026-11-01t02:26:40Z",
        "2026-11-01T02:26:40z",
        "x2026-11-01T02:26:40Z",
    ];
    let no_moment = [
        "2026-02-30T02:26:40Z",
        "2026-11-01T24:00:00Z",
        "2016-12-31T23:59:60Z",
    ];
    let paths = api.document()["paths"].as_object().unwrap().clone();
    let mut filters = 0;

    for (template, item) in &paths {
        let moments = item["get"]["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|parameter| parameter["schema"]["format"] == "date-time");
        for parameter in moments {
            let name = parameter["name"].as_str().unwrap();
            // The pattern alone: the format's own check is the validator's, not the document's.
            let pattern = serde_json::json!({ "pattern": parameter["schema"]["pattern"] });
            let documented = jsonschema::validator_for(&pattern).unwrap();
            let path = template.replace("{workspace_id}", &workspace);
            let forms = taken
                .iter()
                .map(|moment| (moment, true, true))
                .chain(malformed.iter().map(|moment| (moment, false, false)))
                .chain(no_moment.iter().map(|moment| (moment, true, false)));
            for (moment, matches, answers) in forms {
                let value: String =
                    url::form_urlencoded::byte_serialize(moment.as_bytes()).collect();
                let response = api
                    .get(&format!("{path}?{name}={value}"))
                    .bearer(&ada.access)
                    .send()
                    .await;

                assert_eq!(
                    documented.is_valid(&serde_json::json!(moment)),
                    matches,
                    "{template} {name} {moment}"
                );
                assert_eq!(
                    response.status == StatusCode::OK,
                    answers,
                    "{template} {name} {moment}: {}",
                    response.status
                );
            }
            filters += 1;
        }
    }
    assert!(filters >= 2, "only {filters} filters that take a moment");
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
