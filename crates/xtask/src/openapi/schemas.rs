//! `check-openapi`, for the schemas: IDs are `format: uuid`, timestamps `format: date-time`, and
//! closed sets `snake_case` strings (API-11); and every string of a request body has a
//! `maxLength` (API-17).
//!
//! An ID is a property or a parameter named `id` or `<name>_id`, and a timestamp a property
//! named `<event>_at`, as DATA-01 names their columns.

use serde_json::{Map, Value};

use super::{METHODS, Problem, is_snake_case, operations::parameters, quoted};

/// The IDs that aren't UUIDs: a trace ID, which API-11 gives the 32 lowercase hexadecimal digits
/// of W3C Trace Context, and a device ID, which its client chooses (the auth spec's Device IDs).
const NOT_UUIDS: [&str; 2] = ["trace_id", "device_id"];

/// The most references a schema follows before the check gives up on a cycle.
const DEPTH: usize = 16;

/// `value`, or what its `$ref` points at, inside `document`.
pub fn resolve<'a>(document: &'a Value, value: &'a Value) -> &'a Value {
    let mut value = value;
    for _ in 0..DEPTH {
        match value["$ref"]
            .as_str()
            .and_then(|target| target.strip_prefix('#'))
            .and_then(|pointer| document.pointer(pointer))
        {
            Some(target) => value = target,
            None => break,
        }
    }
    value
}

/// Every way the document's schemas break API-11, and its request bodies API-17.
pub fn problems(document: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    let empty = Map::new();
    for (name, schema) in document["components"]["schemas"]
        .as_object()
        .unwrap_or(&empty)
    {
        formats(
            document,
            schema,
            &format!("schema {name}"),
            &quoted(name),
            &mut problems,
        );
    }
    for (path, item) in document["paths"].as_object().unwrap_or(&empty) {
        for method in METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let here = format!("{} {path}", method.to_ascii_uppercase());
            let needle = quoted(path);
            problems.extend(identifiers(
                document,
                path,
                operation,
                &item["parameters"],
                &here,
            ));
            for (media, content) in operation["requestBody"]["content"]
                .as_object()
                .unwrap_or(&empty)
            {
                let owner = format!("{here} {media} request");
                formats(document, &content["schema"], &owner, &needle, &mut problems);
                if media == "application/json" || media.ends_with("+json") {
                    let mut lengths = Lengths {
                        document,
                        owner: &owner,
                        needle: &needle,
                        seen: Vec::new(),
                        problems: Vec::new(),
                    };
                    lengths.walk(&content["schema"], "");
                    problems.extend(lengths.problems);
                }
            }
            for (status, response) in operation["responses"].as_object().unwrap_or(&empty) {
                for (media, content) in response["content"].as_object().unwrap_or(&empty) {
                    // A problem's codes are ERR-02's, not closed sets of API-11.
                    if media != "application/problem+json" {
                        let owner = format!("{here} {status}");
                        formats(document, &content["schema"], &owner, &needle, &mut problems);
                    }
                }
            }
        }
    }
    problems
}

/// API-11: the IDs among an operation's path and query parameters are UUIDs.
fn identifiers(
    document: &Value,
    path: &str,
    operation: &Value,
    shared: &Value,
    here: &str,
) -> Vec<Problem> {
    let parameters = parameters(document, operation, shared);
    let mut names: Vec<String> = path
        .split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .map(str::to_owned)
        .collect();
    for parameter in &parameters {
        let name = parameter["name"].as_str().unwrap_or_default();
        if parameter["in"] == "query" && !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    }
    names
        .into_iter()
        .filter(|name| is_id(name))
        .filter(|name| {
            !parameters.iter().any(|parameter| {
                parameter["name"] == name.as_str()
                    && formats_of(document, &parameter["schema"], 0).contains(&"uuid")
            })
        })
        .map(|name| {
            (
                quoted(path),
                format!("{here}: the parameter {name} isn't documented as format: uuid (API-11)"),
            )
        })
        .collect()
}

/// API-11: the IDs and timestamps among `schema`'s properties, and those of the schemas inside
/// it, have their formats, and its closed sets are `snake_case`. A reference isn't followed:
/// the components it points at are checked on their own.
fn formats(
    document: &Value,
    schema: &Value,
    owner: &str,
    needle: &str,
    problems: &mut Vec<Problem>,
) {
    if let Some(values) = schema["enum"].as_array() {
        for value in values.iter().filter_map(Value::as_str) {
            if !is_snake_case(value) {
                problems.push((
                    needle.to_owned(),
                    format!("{owner}: the value {value} of a closed set isn't snake_case (API-11)"),
                ));
            }
        }
    }
    if let Some(properties) = schema["properties"].as_object() {
        for (name, property) in properties {
            let found = formats_of(document, property, 0);
            if is_id(name) && !NOT_UUIDS.contains(&name.as_str()) && !found.contains(&"uuid") {
                problems.push((
                    quoted(name),
                    format!("{owner}: the ID {name} isn't format: uuid (API-11)"),
                ));
            }
            if name.ends_with("_at") && !found.contains(&"date-time") {
                problems.push((
                    quoted(name),
                    format!("{owner}: the timestamp {name} isn't format: date-time (API-11)"),
                ));
            }
            formats(
                document,
                property,
                &format!("{owner}.{name}"),
                needle,
                problems,
            );
        }
    }
    for key in ["items", "additionalProperties"] {
        if schema[key].is_object() {
            formats(document, &schema[key], owner, needle, problems);
        }
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        for part in schema[key].as_array().into_iter().flatten() {
            formats(document, part, owner, needle, problems);
        }
    }
}

/// The formats `schema` gives its values: its own, or those of its parts other than `null`,
/// with references followed.
fn formats_of<'a>(document: &'a Value, schema: &'a Value, depth: usize) -> Vec<&'a str> {
    let schema = resolve(document, schema);
    let mut found: Vec<&str> = schema["format"].as_str().into_iter().collect();
    if depth < DEPTH {
        for key in ["allOf", "anyOf", "oneOf"] {
            for part in schema[key].as_array().into_iter().flatten() {
                found.extend(formats_of(document, part, depth + 1));
            }
        }
    }
    found
}

/// API-17: every string of a request body has a `maxLength`, unless it is a closed set, whose
/// values bound it.
struct Lengths<'a> {
    document: &'a Value,
    /// The request, as findings name it.
    owner: &'a str,
    /// The text to find a finding's line by.
    needle: &'a str,
    /// The references followed to reach the schema at hand, which a cycle would repeat.
    seen: Vec<String>,
    problems: Vec<Problem>,
}

impl Lengths<'_> {
    /// Checks `schema`, which describes the value at the JSON Pointer `at`.
    fn walk(&mut self, schema: &Value, at: &str) {
        if let Some(target) = schema["$ref"].as_str() {
            if self.seen.iter().any(|seen| seen == target) || self.seen.len() >= DEPTH {
                return;
            }
            self.seen.push(target.to_owned());
            self.walk(resolve(self.document, schema), at);
            self.seen.pop();
            return;
        }
        let is_string = match &schema["type"] {
            Value::String(kind) => kind == "string",
            Value::Array(kinds) => kinds.iter().any(|kind| kind == "string"),
            _ => false,
        };
        let bounded = ["maxLength", "enum", "const"]
            .iter()
            .any(|key| schema.get(*key).is_some());
        if is_string && !bounded {
            let field = if at.is_empty() { "the body" } else { at };
            self.problems.push((
                self.needle.to_owned(),
                format!(
                    "{}: the string at {field} has no maxLength (API-17)",
                    self.owner
                ),
            ));
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (name, property) in properties {
                self.walk(property, &format!("{at}/{name}"));
            }
        }
        for key in ["items", "additionalProperties"] {
            if schema[key].is_object() {
                self.walk(&schema[key], &format!("{at}/*"));
            }
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            for part in schema[key].as_array().into_iter().flatten() {
                self.walk(part, at);
            }
        }
    }
}

/// Whether `name` names an ID: `id`, or `<name>_id`.
fn is_id(name: &str) -> bool {
    name == "id" || name.ends_with("_id")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn messages(document: &Value) -> Vec<String> {
        problems(document)
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    fn document(operation: Value, schemas: Value) -> Value {
        json!({ "paths": { "/v1/things": { "post": operation } }, "components": { "schemas": schemas } })
    }

    #[test]
    fn api_11_ids_are_uuids_and_timestamps_date_times() {
        let schemas = json!({
            "UserId": { "type": "string", "format": "uuid" },
            "Timestamp": { "type": "string", "format": "date-time" },
            "Thing": { "type": "object", "properties": {
                "id": { "$ref": "#/components/schemas/UserId" },
                "owner_id": { "oneOf": [{ "$ref": "#/components/schemas/UserId" }, { "type": "null" }] },
                "created_at": { "$ref": "#/components/schemas/Timestamp" },
                "trace_id": { "type": "string" },
                "device_id": { "type": "string", "maxLength": 64 },
                "parent_id": { "type": "integer" },
                "deleted_at": { "type": "string" },
                "kind": { "type": "string", "enum": ["plain_thing", "fancyThing"] },
            } },
        });
        assert_eq!(
            messages(&document(json!({ "responses": {} }), schemas)),
            [
                "schema Thing: the timestamp deleted_at isn't format: date-time (API-11)",
                "schema Thing.kind: the value fancyThing of a closed set isn't snake_case (API-11)",
                "schema Thing: the ID parent_id isn't format: uuid (API-11)",
            ]
        );
    }

    #[test]
    fn api_11_id_parameters_are_uuids() {
        let document = json!({ "paths": { "/v1/things/{thing_id}": { "get": {
            "parameters": [
                { "name": "thing_id", "in": "path", "schema": { "type": "string" } },
                { "name": "owner_id", "in": "query", "schema": { "type": "string", "format": "uuid" } },
            ],
            "responses": {},
        } } } });
        assert_eq!(
            messages(&document),
            [
                "GET /v1/things/{thing_id}: the parameter thing_id isn't documented as format: uuid (API-11)"
            ]
        );
    }

    #[test]
    fn api_11_a_problem_s_codes_are_no_closed_set() {
        let operation = json!({ "responses": { "400": { "content": { "application/problem+json": {
            "schema": { "properties": { "code": { "enum": ["request.invalid_body"] } } },
        } } } } });
        assert_eq!(
            messages(&document(operation, json!({}))),
            Vec::<String>::new()
        );
    }

    #[test]
    fn api_17_every_string_of_a_request_body_has_a_max_length() {
        let schemas = json!({
            "Name": { "type": "string", "maxLength": 100 },
            "Note": { "type": "string" },
            "Delivery": { "type": "string", "enum": ["body", "cookie"] },
            "Request": { "type": "object", "properties": {
                "name": { "$ref": "#/components/schemas/Name" },
                "note": { "oneOf": [{ "$ref": "#/components/schemas/Note" }, { "type": "null" }] },
                "delivery": { "$ref": "#/components/schemas/Delivery" },
                "tags": { "type": "array", "items": { "type": ["string", "null"] } },
                "token": { "type": ["string", "null"], "maxLength": 64 },
            } },
        });
        let operation = json!({
            "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Request" } } } },
            "responses": {},
        });
        assert_eq!(
            messages(&document(operation, schemas)),
            [
                "POST /v1/things application/json request: the string at /note has no maxLength (API-17)",
                "POST /v1/things application/json request: the string at /tags/* has no maxLength (API-17)",
            ]
        );
    }
}
