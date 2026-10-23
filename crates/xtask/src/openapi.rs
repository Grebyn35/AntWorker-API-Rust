//! `check-openapi`: the committed `openapi.json` follows the rules for the HTTP contract
//! (API-01, API-05, API-17, ERR-02, ERR-13).

use serde_json::{Map, Value};

use crate::{Finding, Outcome, repo::Repo};

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
    for (path, item) in paths {
        problems.extend(path_shape(path));
        for method in METHODS {
            if let Some(operation) = item.get(method) {
                problems.extend(operation_problems(path, method, operation));
            }
        }
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
    for (status, response) in operation["responses"].as_object().unwrap_or(&empty) {
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

/// The line of the first occurrence of `needle` in `text`, or 1.
fn line_of(text: &str, needle: &str) -> usize {
    text.find(needle)
        .map_or(1, |at| text[..at].matches('\n').count() + 1)
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

    fn document(path: &str, operation: Value, schemas: Value) -> Value {
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
                "200": { "description": "The thing." },
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
        let ok = document(
            "/v1/data-exports/{data_export_id}",
            operation(),
            json!({ "Export": { "properties": { "created_at": {} } } }),
        );
        assert_eq!(findings(&ok), Vec::<String>::new());
    }

    #[test]
    fn api_01_paths_are_versioned_kebab_case_with_id_parameters() {
        let messages = findings(&document("/v1/dataExports/{id}", operation(), json!({})));
        assert_eq!(messages.len(), 2, "{messages:#?}");
        assert!(messages.iter().all(|m| m.ends_with("(API-01)")));
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
    fn api_17_every_operation_states_its_access_and_every_response_a_description() {
        let mut bare = operation();
        bare.as_object_mut().unwrap().remove("x-access");
        bare["responses"]["200"] = json!({});
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
