//! The OpenAPI document (API-17).

use std::collections::BTreeSet;

use http::{Method, StatusCode};
use serde_json::Value;

use crate::support::{Api, run_binary, streams, users, workspaces};

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

/// Each documented operation with a path parameter, once for each of its parameters, with
/// `%FF` in that one, which percent-decodes to a byte that no UTF-8 text holds: its method, its
/// path's template, the parameter, the path, and the operation. Every other parameter names
/// `workspace`, or an ID that names no row.
fn not_utf_8_in_each_parameter(
    api: &Api,
    workspace: &str,
) -> Vec<(Method, String, String, String, Value)> {
    let mut requests = Vec::new();
    for (template, item) in api.document()["paths"].as_object().unwrap() {
        let parameters = template.split('/').filter(|part| part.starts_with('{'));
        for parameter in parameters {
            let path: String = template
                .split('/')
                .map(|part| match part {
                    _ if part == parameter => "%FF",
                    "{workspace_id}" => workspace,
                    _ if part.starts_with('{') => "01900000-0000-7000-8000-000000000000",
                    _ => part,
                })
                .collect::<Vec<_>>()
                .join("/");
            for (method, operation) in item.as_object().unwrap() {
                let method = Method::from_bytes(method.to_ascii_uppercase().as_bytes()).unwrap();
                requests.push((
                    method,
                    template.clone(),
                    parameter.to_owned(),
                    path.clone(),
                    operation.clone(),
                ));
            }
        }
    }
    requests
}

#[tokio::test]
async fn api_07_every_route_answers_a_path_segment_that_isn_t_utf_8_with_a_problem() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    let mut sent = 0;

    for (method, template, parameter, path, _) in not_utf_8_in_each_parameter(&api, &workspace) {
        let response = api
            .request(method.clone(), &path)
            .bearer(&ada.access)
            .send()
            .await;

        assert_eq!(
            response.header("content-type"),
            "application/problem+json",
            "{method} {template} with {parameter} not UTF-8: {}",
            response.status
        );
        sent += 1;
    }
    assert!(sent >= 10, "only {sent} requests");
}

#[tokio::test]
async fn platform_072_every_route_answers_a_path_segment_that_isn_t_utf_8_as_a_row_it_doesn_t_find()
{
    let api = Api::new().await;
    // The owner of the workspace, who holds every permission of its routes.
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    let mut sent = 0;

    for (method, template, parameter, path, operation) in
        not_utf_8_in_each_parameter(&api, &workspace)
    {
        // A body that the request lacks is refused before the route reads an ID (API-10).
        if !operation["requestBody"].is_null() {
            continue;
        }
        let response = api
            .request(method.clone(), &path)
            .bearer(&ada.access)
            .send()
            .await;

        let expected = StatusCode::NOT_FOUND;
        // part: superadmins
        // A route of an access level that she lacks refuses her first.
        let expected = if operation["x-access"] == "superadmin" {
            StatusCode::FORBIDDEN
        } else {
            expected
        };
        // end part: superadmins
        assert_eq!(
            response.status,
            expected,
            "{method} {template} with {parameter} not UTF-8: {}",
            response.json()
        );
        if expected == StatusCode::FORBIDDEN {
            assert_eq!(response.json()["code"], "request.permission_denied");
        }
        sent += 1;
    }
    assert!(sent >= 10, "only {sent} requests");
}

#[tokio::test]
async fn sec_12_every_documented_route_answers_with_the_security_headers() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // What ends Ada's session or her account goes last, so that every other request is hers.
    let mut operations = api.operations();
    operations.sort_by_key(|(method, path, _)| {
        *method == Method::DELETE
            && matches!(
                path.as_str(),
                "/v1/users/me" | "/v1/users/me/sessions/current"
            )
    });
    let mut answers = Vec::new();

    for (method, path, _) in operations {
        if path == streams::PATH {
            let stream = streams::connect(&api, &ada.access, None).await;
            answers.push((method, path, stream.headers().clone()));
            continue;
        }
        let response = api
            .request(method.clone(), &path)
            .bearer(&ada.access)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await;
        answers.push((method, path, response.headers));
    }

    for (method, path, headers) in &answers {
        let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
        assert_eq!(
            header("x-content-type-options"),
            Some("nosniff"),
            "{method} {path}"
        );
        assert_eq!(
            header("referrer-policy"),
            Some("no-referrer"),
            "{method} {path}"
        );
        assert_eq!(header("cache-control"), Some("no-store"), "{method} {path}");
        assert_eq!(
            header("content-security-policy"),
            Some("default-src 'none'; base-uri 'none'; frame-ancestors 'none'"),
            "{method} {path}"
        );
    }
    assert!(answers.len() >= 20, "only {} routes", answers.len());
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
            assert_eq!(
                response.json()["code"],
                "request.invalid_query",
                "{template}"
            );
            let fields: Vec<String> = response
                .refused_parameters()
                .into_iter()
                .map(|(field, _)| field)
                .collect();
            assert_eq!(fields, [format!("/{name}")], "{template}");
            sent += 1;
        }
    }
    assert!(sent >= 6, "only {sent} parameters");
}

/// `schema`, a request body's or a field's, with its `$ref` followed, and, of a `oneOf` of two
/// members, one of them `null`, as utoipa writes an `Option` of a type of its own, the other.
fn resolved<'a>(document: &'a Value, schema: &'a Value) -> &'a Value {
    let schema = match schema["oneOf"].as_array() {
        Some(members) => {
            let others: Vec<&Value> = members.iter().filter(|m| m["type"] != "null").collect();
            assert!(members.len() == 2 && others.len() == 1, "{schema}");
            others[0]
        }
        None => schema,
    };
    match schema["$ref"].as_str() {
        Some(reference) => {
            let name = reference.trim_start_matches("#/components/schemas/");
            &document["components"]["schemas"][name]
        }
        None => schema,
    }
}

/// The values that API-10's generated test sends a field of a JSON request body whose schema is
/// `field`: `a` repeated once more than its `maxLength`, a value of its `maxLength` followed by a
/// space, which a type that trims would take, `a` repeated, or, for the format `email`, an
/// address at `example.com`, and `a` repeated once fewer than its `minLength`, when that is above
/// 0, and, for a `pattern` or an `enum`, U+0001 repeated as often as its `minLength`, or once. The
/// test checks that the field's schema refuses each.
fn outside_its_bounds(field: &Value) -> Vec<String> {
    let length = |key: &str| field[key].as_u64().map(|n| usize::try_from(n).unwrap());
    let mut values = Vec::new();
    if let Some(max) = length("maxLength") {
        values.push("a".repeat(max + 1));
        let at_bound = if field["format"] == "email" {
            format!("{}@example.com", "a".repeat(max - "@example.com".len()))
        } else {
            "a".repeat(max)
        };
        values.push(format!("{at_bound} "));
    }
    if let Some(min) = length("minLength").filter(|min| *min > 0) {
        values.push("a".repeat(min - 1));
    }
    if !field["pattern"].is_null() || !field["enum"].is_null() {
        values.push("\u{1}".repeat(length("minLength").unwrap_or(1).max(1)));
    }
    values
}

/// A field of a JSON request body of the document, with what a request to it needs.
struct BodyField {
    method: Method,
    template: String,
    access: Value,
    media: String,
    /// The body's schema.
    body: Value,
    name: String,
    /// The field's schema.
    field: Value,
}

/// Each property of the schema of each request body of `document` whose media type, without its
/// parameters, ends in `json`, the body's schema and the property's as [`resolved`] reads them.
/// It fails on a body's schema that holds a keyword of [`UNREAD_FORMS`], and on a property's that
/// holds one of those or of [`UNREAD_BOUNDS`].
fn json_body_fields(document: &Value) -> Vec<BodyField> {
    let mut fields = Vec::new();
    for (template, item) in document["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            let contents = operation["requestBody"]["content"].as_object();
            let json = contents.into_iter().flatten().filter(|(media, _)| {
                media
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .ends_with("json")
            });
            for (media, content) in json {
                let body = resolved(document, &content["schema"]);
                for unread in UNREAD_FORMS {
                    assert!(body[unread].is_null(), "{template}: {unread}");
                }
                for (name, field) in body["properties"].as_object().into_iter().flatten() {
                    let field = resolved(document, field);
                    for unread in UNREAD_FORMS.iter().chain(&UNREAD_BOUNDS) {
                        assert!(field[unread].is_null(), "{template} {name}: {unread}");
                    }
                    fields.push(BodyField {
                        method: Method::from_bytes(method.to_ascii_uppercase().as_bytes()).unwrap(),
                        template: template.clone(),
                        access: operation["x-access"].clone(),
                        media: media.clone(),
                        body: body.clone(),
                        name: name.clone(),
                        field: field.clone(),
                    });
                }
            }
        }
    }
    fields
}

/// The keywords of a schema that [`resolved`] leaves, and that [`json_body_fields`] doesn't read.
const UNREAD_FORMS: [&str; 5] = ["$ref", "allOf", "anyOf", "oneOf", "not"];

/// The keywords of a field's schema that [`outside_its_bounds`] doesn't read: a number's bounds,
/// `const`, an array's bounds and items, and an object's properties.
const UNREAD_BOUNDS: [&str; 9] = [
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "const",
    "minItems",
    "maxItems",
    "items",
    "properties",
];

/// The body that gives `field` the value `value`, and each other field that its body requires
/// `false`, if its schema's type is `boolean`, or else `""`, which the conversion may refuse too.
fn body_with(document: &Value, field: &BodyField, value: &str) -> String {
    let required = field.body["required"].as_array().into_iter().flatten();
    let mut body: serde_json::Map<String, Value> = required
        .filter_map(Value::as_str)
        .map(|other| {
            let kind = &resolved(document, &field.body["properties"][other])["type"];
            let blank = if *kind == "boolean" {
                Value::Bool(false)
            } else {
                Value::from("")
            };
            (other.to_owned(), blank)
        })
        .collect();
    body.insert(field.name.clone(), Value::from(value));
    Value::Object(body).to_string()
}

#[tokio::test]
async fn api_10_every_bound_of_a_json_body_s_field_refuses_a_value_outside_it_at_the_field() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    let mut sent = 0;

    for field in json_body_fields(api.document()) {
        let path = field
            .template
            .split('/')
            .map(|part| match part {
                "{workspace_id}" => workspace.as_str(),
                _ if part.starts_with('{') => "01900000-0000-7000-8000-000000000000",
                _ => part,
            })
            .collect::<Vec<_>>()
            .join("/");
        for value in outside_its_bounds(&field.field) {
            let schema = jsonschema::validator_for(&field.field).unwrap();
            assert!(
                !schema.is_valid(&Value::from(value.as_str())),
                "{} {value:?}",
                field.name
            );
            let call = api
                .request(field.method.clone(), &path)
                .header("content-type", &field.media)
                .body(body_with(api.document(), &field, &value));
            let call = if field.access == "public" {
                call
            } else {
                call.bearer(&ada.access)
            };

            let response = call.send().await;

            let at = format!(
                "{} {} /{} {value:?}",
                field.method, field.template, field.name
            );
            assert_eq!(
                response.status,
                StatusCode::BAD_REQUEST,
                "{at}: {}",
                String::from_utf8_lossy(&response.body)
            );
            let refused: Vec<String> = response
                .refused_fields()
                .into_iter()
                .map(|(refused, _)| refused)
                .collect();
            assert!(
                refused.contains(&format!("/{}", field.name)),
                "{at}: {refused:?}"
            );
            sent += 1;
        }
    }
    assert!(sent >= 100, "only {sent} values");
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
