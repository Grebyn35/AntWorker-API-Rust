//! `check-openapi`: the committed `openapi.json` follows the rules for the HTTP contract
//! (API-01 to API-06, API-11, API-17, ERR-02, ERR-13). Each operation's scope, method and
//! statuses are in `operations`, the shape of lists in `lists`, and the formats and request
//! strings' lengths in `schemas`.

mod lists;
mod operations;
mod schemas;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::{Finding, Outcome, repo::Repo, text::line_of};

/// The committed document.
const DOCUMENT: &str = "openapi.json";

/// The access levels of API-14.
const ACCESS_LEVELS: [&str; 3] = ["public", "user", "workspace"];

/// The keys of a path item that hold operations.
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Checks the committed document.
pub fn check(repo: &Repo) -> Outcome {
    let Some(file) = repo.file(DOCUMENT) else {
        return Outcome {
            report: Vec::new(),
            findings: vec![Finding::new(
                DOCUMENT,
                1,
                "the document is missing (API-17)",
            )],
        };
    };
    let document: Value = match serde_json::from_str(&file.text) {
        Ok(document) => document,
        Err(error) => {
            return Outcome {
                report: Vec::new(),
                findings: vec![Finding::new(DOCUMENT, error.line(), error.to_string())],
            };
        }
    };
    let mut problems = Vec::new();
    let empty = Map::new();
    let paths = document["paths"].as_object().unwrap_or(&empty);
    let mut groups: Vec<(String, String, String, BTreeSet<String>)> = Vec::new();
    for (path, item) in paths {
        problems.extend(path_shape(path));
        for method in METHODS {
            if let Some(operation) = item.get(method) {
                problems.extend(operation_problems(path, method, operation));
                groups.push((
                    format!("{} {path}", method.to_ascii_uppercase()),
                    quoted(path),
                    group(method, operation),
                    request_codes(operation),
                ));
            }
        }
    }
    problems.extend(group_codes(&groups));
    problems.extend(references(&document, &document));
    problems.extend(operations::check(&document));
    problems.extend(lists::check(&document));
    problems.extend(schemas::problems(&document));
    let responses = document["components"]["responses"]
        .as_object()
        .unwrap_or(&empty);
    for (name, response) in responses {
        let here = format!("the response {name}");
        let description = response["description"].as_str().unwrap_or_default();
        if description.trim().is_empty() {
            problems.push((
                quoted(name),
                format!("{here} has no description, which OpenAPI requires (API-17)"),
            ));
        }
        problems.extend(error_codes(
            &here,
            &quoted(name),
            name,
            response,
            description,
        ));
    }
    let schemas = document["components"]["schemas"]
        .as_object()
        .unwrap_or(&empty);
    for (name, schema) in schemas {
        properties(&format!("schema {name}"), schema, &mut problems);
    }
    let findings = problems
        .into_iter()
        .map(|(needle, message)| Finding::new(DOCUMENT, line_of(&file.text, &needle), message))
        .collect();
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// A problem, with the text to find its line by.
type Problem = (String, String);

/// API-01: a path is under `/v1`, its segments are lowercase kebab-case, and its parameters are
/// named `{<resource>_id}`.
fn path_shape(path: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    let Some(rest) = path.strip_prefix("/v1/") else {
        return vec![(quoted(path), format!("{path} isn't under /v1 (API-01)"))];
    };
    for segment in rest.split('/') {
        let fine = if let Some(name) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            name.ends_with("_id") && is_snake_case(name)
        } else {
            !segment.is_empty()
                && segment.split('-').all(|word| {
                    !word.is_empty()
                        && word
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                })
        };
        if !fine {
            problems.push((
                quoted(path),
                format!("{path}: the segment {segment} isn't kebab-case or a {{<resource>_id}} parameter (API-01)"),
            ));
        }
    }
    problems
}

/// API-17, ERR-02 and ERR-13: the operation states its access level, every response has a
/// description, and every error response lists its codes.
fn operation_problems(path: &str, method: &str, operation: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    let here = format!("{} {path}", method.to_ascii_uppercase());
    let needle = quoted(path);
    match operation["x-access"].as_str() {
        Some(access) if ACCESS_LEVELS.contains(&access) => {}
        _ => problems.push((
            needle.clone(),
            format!("{here} doesn't state its access level in x-access (API-17)"),
        )),
    }
    let empty = Map::new();
    for (media, content) in operation["requestBody"]["content"]
        .as_object()
        .unwrap_or(&empty)
    {
        properties(
            &format!("{here} {media} request"),
            &content["schema"],
            &mut problems,
        );
    }
    for (status, response) in operation["responses"].as_object().unwrap_or(&empty) {
        for content in response["content"].as_object().unwrap_or(&empty).values() {
            properties(
                &format!("{here} {status}"),
                &content["schema"],
                &mut problems,
            );
        }
        let description = response["description"].as_str().unwrap_or_default();
        if description.trim().is_empty() {
            problems.push((
                needle.clone(),
                format!(
                    "{here} answers {status} with no description, which OpenAPI requires (API-17)"
                ),
            ));
        }
        if status.starts_with('4') || status.starts_with('5') {
            problems.extend(error_codes(&here, &needle, status, response, description));
        }
    }
    problems
}

/// ERR-02 and ERR-13: an error response is a problem that lists its codes, each named
/// `<namespace>.<snake_case_name>`, and its description gives each code's meaning.
fn error_codes(
    here: &str,
    needle: &str,
    status: &str,
    response: &Value,
    description: &str,
) -> Vec<Problem> {
    let schema = &response["content"]["application/problem+json"]["schema"];
    let codes: Vec<&str> = schema["allOf"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["properties"]["code"]["enum"].as_array())
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if codes.is_empty() {
        return vec![(
            needle.to_owned(),
            format!("{here} answers {status} without a problem that lists its codes (ERR-13)"),
        )];
    }
    let mut problems = Vec::new();
    for code in codes {
        let named = code
            .split_once('.')
            .is_some_and(|(namespace, name)| is_snake_case(namespace) && is_snake_case(name));
        if !named {
            problems.push((
                needle.to_owned(),
                format!("{here}: the code {code} isn't <namespace>.<snake_case_name> (ERR-02)"),
            ));
        }
        if !description.contains(&format!("`{code}`")) {
            problems.push((
                needle.to_owned(),
                format!("{here}: the {status} response doesn't say what {code} means (ERR-13)"),
            ));
        }
    }
    problems
}

/// The `request.` codes an operation lists: the problems `platform` answers on any route of
/// the operation's group.
fn request_codes(operation: &Value) -> BTreeSet<String> {
    let empty = Map::new();
    operation["responses"]
        .as_object()
        .unwrap_or(&empty)
        .values()
        .flat_map(|response| {
            response["content"]["application/problem+json"]["schema"]["allOf"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| part["properties"]["code"]["enum"].as_array())
                .flatten()
                .filter_map(Value::as_str)
        })
        .filter(|code| code.starts_with("request."))
        .map(str::to_owned)
        .collect()
}

/// The operations whose pipeline answers the same `request.` codes: those of one access level,
/// split by whether their method is safe, since the cross-site check refuses only a request that
/// changes something, and whether they take a body, whose problems only they answer.
fn group(method: &str, operation: &Value) -> String {
    let access = operation["x-access"].as_str().unwrap_or_default();
    let kind = if operation.get("requestBody").is_some() {
        "body-taking "
    } else if ["get", "head", "options", "trace"].contains(&method) {
        ""
    } else {
        "changing "
    };
    format!("{kind}{access}")
}

/// ERR-13: an operation lists every `request.` code that another operation of its group lists,
/// since the pipeline answers each of them on any route of the group.
fn group_codes(operations: &[(String, String, String, BTreeSet<String>)]) -> Vec<Problem> {
    let mut by_access: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (_, _, access, codes) in operations {
        by_access
            .entry(access)
            .or_default()
            .extend(codes.iter().map(String::as_str));
    }
    let mut problems = Vec::new();
    for (here, needle, access, codes) in operations {
        for code in &by_access[access.as_str()] {
            if !codes.contains(*code) {
                problems.push((
                    needle.clone(),
                    format!(
                        "{here} doesn't list {code}, which another {access} route answers (ERR-13)"
                    ),
                ));
            }
        }
    }
    problems
}

/// API-05: every property of `schema`, and of the schemas inside it, is `snake_case`.
fn properties(owner: &str, schema: &Value, problems: &mut Vec<Problem>) {
    if let Some(members) = schema["properties"].as_object() {
        for (name, property) in members {
            if !is_snake_case(name) {
                problems.push((
                    quoted(name),
                    format!("{owner}: the property {name} isn't snake_case (API-05)"),
                ));
            }
            properties(&format!("{owner}.{name}"), property, problems);
        }
    }
    for key in ["items", "additionalProperties"] {
        if schema[key].is_object() {
            properties(owner, &schema[key], problems);
        }
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        for part in schema[key].as_array().into_iter().flatten() {
            properties(owner, part, problems);
        }
    }
}

/// API-17: every `$ref` in `value` points at something inside `document`, so a route can't
/// document a body with a schema the document doesn't hold.
fn references(document: &Value, value: &Value) -> Vec<Problem> {
    match value {
        Value::Object(members) => members
            .iter()
            .flat_map(|(key, member)| match (key.as_str(), member) {
                ("$ref", Value::String(target)) => {
                    let resolves = target
                        .strip_prefix('#')
                        .is_some_and(|pointer| document.pointer(pointer).is_some());
                    if resolves {
                        Vec::new()
                    } else {
                        vec![(
                            quoted(target),
                            format!("the reference {target} doesn't resolve (API-17)"),
                        )]
                    }
                }
                _ => references(document, member),
            })
            .collect(),
        Value::Array(items) => items
            .iter()
            .flat_map(|item| references(document, item))
            .collect(),
        _ => Vec::new(),
    }
}

fn is_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.split('_').all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

fn quoted(text: &str) -> String {
    format!("\"{text}\"")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn findings(document: &Value) -> Vec<String> {
        let repo = Repo::of(&[(DOCUMENT, &document.to_string())]);
        check(&repo)
            .findings
            .into_iter()
            .map(|finding| finding.message)
            .collect()
    }

    /// A document with one `GET` at `path`, and `schemas` beside the `Problem` that every
    /// error response refers to.
    fn document(path: &str, operation: Value, mut schemas: Value) -> Value {
        schemas["Problem"] = json!({ "type": "object" });
        json!({
            "openapi": "3.1.0",
            "paths": { path: { "get": operation } },
            "components": { "schemas": schemas },
        })
    }

    fn operation() -> Value {
        json!({
            "x-access": "public",
            "responses": {
                "200": {
                    "description": "The thing.",
                    "content": { "application/json": { "schema": {} } },
                },
                "503": {
                    "description": "`request.timed_out`: Request timed out.",
                    "content": { "application/problem+json": { "schema": { "allOf": [
                        { "$ref": "#/components/schemas/Problem" },
                        { "properties": { "code": { "enum": ["request.timed_out"] } } },
                    ] } } },
                },
            },
        })
    }

    #[test]
    fn api_17_a_document_that_follows_the_rules_passes() {
        let mut get = operation();
        get["parameters"] = json!([{
            "name": "data_export_id", "in": "path", "schema": { "type": "string", "format": "uuid" },
        }]);
        let ok = document(
            "/v1/data-exports/{data_export_id}",
            get,
            json!({ "Export": { "properties": { "created_at": {
                "type": "string", "format": "date-time",
            } } } }),
        );
        assert_eq!(findings(&ok), Vec::<String>::new());
    }

    #[test]
    fn api_01_paths_are_versioned_kebab_case_with_id_parameters() {
        let messages = findings(&document("/v1/dataExports/{id}", operation(), json!({})));
        let api_01: Vec<&String> = messages
            .iter()
            .filter(|m| m.ends_with("(API-01)"))
            .collect();
        assert_eq!(api_01.len(), 2, "{messages:#?}");
        let unversioned = findings(&document("/things", operation(), json!({})));
        assert_eq!(unversioned, ["/things isn't under /v1 (API-01)"]);
    }

    #[test]
    fn api_05_properties_are_snake_case() {
        let schemas = json!({ "Export": { "properties": { "createdAt": {}, "items": {
            "type": "array", "items": { "properties": { "Size": {} } },
        } } } });
        let messages = findings(&document("/v1/exports", operation(), schemas));
        assert_eq!(
            messages,
            [
                "schema Export: the property createdAt isn't snake_case (API-05)",
                "schema Export.items: the property Size isn't snake_case (API-05)",
            ]
        );
    }

    #[test]
    fn api_05_properties_of_inline_schemas_are_snake_case() {
        let mut inline = operation();
        inline["responses"]["200"]["content"] = json!({ "application/json": { "schema": {
            "type": "object", "properties": { "nextCursor": {} },
        } } });
        let messages = findings(&document("/v1/things", inline, json!({})));
        assert_eq!(
            messages,
            ["GET /v1/things 200: the property nextCursor isn't snake_case (API-05)"]
        );
    }

    #[test]
    fn api_17_every_operation_states_its_access_and_every_response_a_description() {
        let mut bare = operation();
        bare.as_object_mut().unwrap().remove("x-access");
        bare["responses"]["200"] = json!({ "content": { "application/json": { "schema": {} } } });
        let messages = findings(&document("/v1/things", bare, json!({})));
        assert_eq!(
            messages,
            [
                "GET /v1/things doesn't state its access level in x-access (API-17)",
                "GET /v1/things answers 200 with no description, which OpenAPI requires (API-17)",
            ]
        );
    }

    #[test]
    fn err_13_every_error_response_lists_its_codes_and_their_meaning() {
        let mut undocumented = operation();
        undocumented["responses"]["503"]["description"] = json!("Unavailable.");
        undocumented["responses"]["404"] = json!({ "description": "Not found." });
        let messages = findings(&document("/v1/things", undocumented, json!({})));
        assert_eq!(
            messages,
            [
                "GET /v1/things answers 404 without a problem that lists its codes (ERR-13)",
                "GET /v1/things: the 503 response doesn't say what request.timed_out means (ERR-13)",
            ]
        );
    }

    #[test]
    fn err_13_every_operation_lists_the_codes_its_route_group_can_answer() {
        let mut full = operation();
        full["responses"]["503"] = json!({
            "description": "`request.timed_out`: Request timed out. `request.overloaded`: Too many requests in progress.",
            "content": { "application/problem+json": { "schema": { "allOf": [
                { "$ref": "#/components/schemas/Problem" },
                { "properties": { "code": { "enum": ["request.timed_out", "request.overloaded"] } } },
            ] } } },
        });
        let mut two = document("/v1/things", operation(), json!({}));
        two["paths"]["/v1/others"] = json!({ "get": full });
        let messages = findings(&two);
        assert_eq!(
            messages,
            [
                "GET /v1/things doesn't list request.overloaded, which another public route answers (ERR-13)"
            ]
        );
    }

    #[test]
    fn err_13_a_route_that_takes_a_body_is_grouped_with_those_that_do() {
        let mut body = operation();
        body["requestBody"] = json!({ "content": {} });
        body["responses"]["400"] = json!({
            "description": "`request.invalid_body`: Invalid body.",
            "content": { "application/problem+json": { "schema": { "allOf": [
                { "$ref": "#/components/schemas/Problem" },
                { "properties": { "code": { "enum": ["request.invalid_body"] } } },
            ] } } },
        });
        let mut without = operation();
        without["requestBody"] = json!({ "content": {} });
        let mut document = document("/v1/things", operation(), json!({}));
        document["paths"]["/v1/things"]["post"] = body;
        document["paths"]["/v1/others"] = json!({ "post": without });
        let messages = findings(&document);
        assert_eq!(
            messages,
            [
                "POST /v1/others doesn't list request.invalid_body, which another body-taking public route answers (ERR-13)"
            ],
            "the GET, which takes no body, needn't list it"
        );
    }

    #[test]
    fn err_13_the_router_s_own_problems_list_their_codes() {
        let mut fallback = document("/v1/things", operation(), json!({}));
        fallback["components"]["responses"] = json!({
            "NotFound": { "description": "Not found." },
        });
        let messages = findings(&fallback);
        assert_eq!(
            messages,
            [
                "the response NotFound answers NotFound without a problem that lists its codes (ERR-13)"
            ]
        );
    }

    #[test]
    fn api_17_every_reference_resolves() {
        let mut dangling = operation();
        dangling["responses"]["200"]["content"] = json!({ "application/json": { "schema": {
            "$ref": "#/components/schemas/Missing",
        } } });
        let messages = findings(&document("/v1/things", dangling, json!({})));
        assert_eq!(
            messages,
            ["the reference #/components/schemas/Missing doesn't resolve (API-17)"]
        );
    }

    #[test]
    fn err_02_codes_are_namespaced_snake_case() {
        let mut badly_named = operation();
        badly_named["responses"]["503"] = json!({
            "description": "`TimedOut`: Request timed out.",
            "content": { "application/problem+json": { "schema": { "allOf": [
                { "properties": { "code": { "enum": ["TimedOut"] } } },
            ] } } },
        });
        let messages = findings(&document("/v1/things", badly_named, json!({})));
        assert_eq!(
            messages,
            ["GET /v1/things: the code TimedOut isn't <namespace>.<snake_case_name> (ERR-02)"]
        );
    }
}
