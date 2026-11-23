//! `check-openapi`, for the schema of a header (API-17): a header has a value or is absent, and is
//! never `null`, so the check refuses a header whose schema it can't show to exclude `null`. A
//! header is a parameter whose `in` is `header`, and a member of `components/headers` and of the
//! `headers` of a response and of an encoding, at each place that [`schemas`](super::schemas)
//! reads. The check reads its `schema` and the `schema` of each content type of its `content`,
//! and refuses it where it has neither. A schema excludes `null` where its `type` is given and
//! neither is nor holds `null`, its `enum` is given and doesn't hold it, its `const` is given and
//! isn't it, each member of its `oneOf`, or each of its `anyOf`, excludes it, of which it has one
//! at least, or a member of its `allOf` excludes it, or what its `$ref` names does.

use serde_json::Value;

use super::{Problem, schemas::DEPTH};

/// The finding for `header`, which findings name as `at`, where the check can't show that its
/// schema excludes `null`.
pub fn problems(document: &Value, header: &Value, at: &str, needle: &str) -> Vec<Problem> {
    let mut schemas: Vec<&Value> = header.get("schema").into_iter().collect();
    let content = header["content"].as_object().into_iter().flatten();
    schemas.extend(content.map(|(_, media)| &media["schema"]));
    if !schemas.is_empty()
        && schemas
            .iter()
            .all(|schema| excludes_null(document, schema, 0))
    {
        return Vec::new();
    }
    vec![(
        needle.to_owned(),
        format!(
            "{at}: a header is never null, and the check can't show that the header's schema \
             excludes null (API-17)"
        ),
    )]
}

/// Whether `schema` excludes `null`, by what the module's comment says it reads.
fn excludes_null(document: &Value, schema: &Value, depth: usize) -> bool {
    if depth > DEPTH {
        return false;
    }
    let inner = |schema: &Value| excludes_null(document, schema, depth + 1);
    let named = schema["$ref"]
        .as_str()
        .and_then(|target| target.strip_prefix('#'))
        .and_then(|pointer| document.pointer(pointer));
    let typed = match &schema["type"] {
        Value::String(kind) => kind != "null",
        Value::Array(kinds) => !kinds.iter().any(|kind| kind == "null"),
        _ => false,
    };
    let each = |key: &str| {
        schema[key]
            .as_array()
            .is_some_and(|members| !members.is_empty() && members.iter().all(inner))
    };
    typed
        || schema["enum"]
            .as_array()
            .is_some_and(|values| !values.contains(&Value::Null))
        || schema.get("const").is_some_and(|value| !value.is_null())
        || each("oneOf")
        || each("anyOf")
        || schema["allOf"].as_array().into_iter().flatten().any(inner)
        || named.is_some_and(inner)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn messages(document: &Value) -> Vec<String> {
        super::super::schemas::problems(document)
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    /// The message for the header at `at`.
    fn refused(at: &str) -> String {
        format!(
            "{at}: a header is never null, and the check can't show that the header's schema \
             excludes null (API-17)"
        )
    }

    #[test]
    fn api_17_a_header_whose_schema_may_be_null_is_refused() {
        let document = json!({
            "paths": { "/v1/things": { "post": {
                "parameters": [
                    { "name": "If-Match", "in": "header", "schema": { "type": ["string", "null"] } },
                    { "name": "Idempotency-Key", "in": "header", "schema": {
                        "oneOf": [{ "type": "string" }, { "type": "null" }] } },
                    { "name": "X-Any", "in": "header", "schema": {
                        "anyOf": [{ "type": "string" }, { "pattern": "^a$" }] } },
                    { "name": "X-None", "in": "header" },
                    { "name": "X-Text", "in": "header", "content": { "text/plain": {} } },
                    { "name": "view", "in": "query", "schema": { "type": ["string", "null"] } },
                    { "name": "X-Kind", "in": "header", "schema": { "enum": ["a", "b"] } },
                    { "name": "X-Key", "in": "header", "schema": { "$ref": "#/components/schemas/Key" } },
                    { "name": "X-Both", "in": "header", "schema": {
                        "allOf": [{ "pattern": "^a$" }, { "type": "string" }] } },
                    { "name": "X-Either", "in": "header", "schema": {
                        "anyOf": [{ "type": "string" }, { "enum": ["a"] }] } },
                    { "name": "X-Empty", "in": "header", "schema": { "oneOf": [] } },
                    { "name": "X-Types", "in": "header", "schema": { "type": ["string", "integer"] } },
                ],
                "responses": { "200": {
                    "description": "x",
                    "headers": {
                        "etag": { "schema": { "type": "string" } },
                        "location": { "schema": { "const": null } },
                    },
                } },
            } } },
            "components": {
                "schemas": { "Key": { "oneOf": [{ "type": "string" }, { "const": "a" }] } },
                "headers": { "X-Anything": { "schema": {} } },
            },
        });

        assert_eq!(
            messages(&document),
            [
                refused("components/headers/X-Anything"),
                refused("POST /v1/things parameter If-Match"),
                refused("POST /v1/things parameter Idempotency-Key"),
                refused("POST /v1/things parameter X-Any"),
                refused("POST /v1/things parameter X-None"),
                refused("POST /v1/things parameter X-Text"),
                refused("POST /v1/things parameter X-Empty"),
                refused("POST /v1/things 200 header location"),
            ]
        );
    }
}
