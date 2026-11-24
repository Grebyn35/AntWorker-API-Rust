//! `check-openapi`, for the fields that a response always holds (API-05): in a schema that no
//! request gives, a property that may be `null` is `required`.
//!
//! The schemas it reads are those under the `responses` of an operation, and each component but
//! those that a request gives: the ones that a `$ref` under an operation's `requestBody` or
//! `parameters`, or under its path's `parameters`, names, and the ones that a `$ref` inside such
//! a component names. A component that a request gives and that a `$ref` under an operation's
//! `responses` names too, or a `$ref` inside a component so named, fails if it has a property
//! that may be `null` and isn't `required`, since a request may leave such a field out and a
//! response holds it. A property may be `null` when its `type` is `null` or holds it, or when one
//! of its `oneOf` is of the type `null`. The check reads the properties of an object, of an
//! array's `items`, and of each member of an `allOf`, a `oneOf` or an `anyOf`, at any depth.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use super::{METHODS, Problem, quoted};

/// The start of a reference to a component that is a schema.
const SCHEMAS: &str = "#/components/schemas/";

/// Each property that may be `null`, and isn't required, of a schema that no request gives, and
/// each component with such a property that a request gives and a response answers with.
pub fn check(document: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    let (mut asked, mut answered) = (Vec::new(), Vec::new());
    let empty = Map::new();
    for (path, item) in document["paths"].as_object().unwrap_or(&empty) {
        references(&item["parameters"], &mut asked);
        for method in METHODS {
            let operation = &item[method];
            references(&operation["requestBody"], &mut asked);
            references(&operation["parameters"], &mut asked);
            let responses = operation["responses"].as_object().unwrap_or(&empty);
            for (status, response) in responses {
                let here = format!("{} {path} {status}", method.to_ascii_uppercase());
                for content in response["content"].as_object().unwrap_or(&empty).values() {
                    unrequired(&here, &quoted(path), &content["schema"], &mut problems);
                }
                references(response, &mut answered);
            }
        }
    }
    let (asked, answered) = (closure(document, asked), closure(document, answered));
    for (name, schema) in document["components"]["schemas"]
        .as_object()
        .unwrap_or(&empty)
    {
        let owner = format!("schema {name}");
        if !asked.contains(name) {
            unrequired(&owner, &quoted(name), schema, &mut problems);
            continue;
        }
        let mut nullable = Vec::new();
        unrequired(&owner, &quoted(name), schema, &mut nullable);
        if answered.contains(name) && !nullable.is_empty() {
            problems.push((
                quoted(name),
                format!(
                    "{owner} has a property that may be null and isn't required, and both a \
                     request and a response refer to it: a request may leave the field out, and a \
                     response holds it, so each takes a type of its own (API-05)"
                ),
            ));
        }
    }
    problems
}

/// `names`, with the name of each component that a `$ref` inside one of them names, through any
/// number of others.
fn closure(document: &Value, mut names: Vec<String>) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    while let Some(name) = names.pop() {
        if found.insert(name.clone()) {
            references(&document["components"]["schemas"][&name], &mut names);
        }
    }
    found
}

/// Adds to `names` the name of each component that a `$ref` anywhere in `value` names: the text
/// after [`SCHEMAS`] of a member `$ref` whose value starts with it. A text under another key,
/// such as a `description`'s or an `example`'s, names none.
fn references(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::Array(items) => items.iter().for_each(|item| references(item, names)),
        Value::Object(members) => {
            for (key, member) in members {
                match member.as_str().and_then(|text| text.strip_prefix(SCHEMAS)) {
                    Some(name) if key == "$ref" => names.push(name.to_owned()),
                    _ => references(member, names),
                }
            }
        }
        _ => {}
    }
}

/// Adds a problem for each property of `schema`, and of the schemas it holds, that may be `null`
/// and isn't among its object's `required`.
fn unrequired(owner: &str, needle: &str, schema: &Value, problems: &mut Vec<Problem>) {
    if !schema.is_object() {
        return;
    }
    let required: Vec<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    for (name, property) in schema["properties"].as_object().into_iter().flatten() {
        if nullable(property) && !required.contains(&name.as_str()) {
            problems.push((
                needle.to_owned(),
                format!(
                    "{owner}: the property {name} may be null, so every response holds it, and \
                     it isn't required (API-05)"
                ),
            ));
        }
        unrequired(&format!("{owner}.{name}"), needle, property, problems);
    }
    unrequired(
        &format!("{owner}.items"),
        needle,
        &schema["items"],
        problems,
    );
    for key in ["allOf", "oneOf", "anyOf"] {
        for member in schema[key].as_array().into_iter().flatten() {
            unrequired(owner, needle, member, problems);
        }
    }
}

/// Whether `schema` takes `null`: its `type` is it or holds it, or one of its `oneOf` is of it.
fn nullable(schema: &Value) -> bool {
    let null = |schema: &Value| match &schema["type"] {
        Value::String(kind) => kind == "null",
        Value::Array(kinds) => kinds.iter().any(|kind| kind == "null"),
        _ => false,
    };
    null(schema) || schema["oneOf"].as_array().into_iter().flatten().any(null)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The messages for a document whose `GET /v1/things` answers 200 with `schema`, and whose
    /// `POST` takes `NewThing` as its body, beside the components `schemas`.
    fn messages(schema: &Value, schemas: &Value) -> Vec<String> {
        let document = json!({
            "paths": { "/v1/things": {
                "get": { "responses": { "200": { "content": { "application/json": {
                    "schema": schema,
                } } } } },
                "post": {
                    "requestBody": { "content": { "application/json": { "schema": {
                        "$ref": "#/components/schemas/NewThing",
                    } } } },
                    "responses": { "204": { "description": "Done." } },
                },
            } },
            "components": { "schemas": schemas },
        });
        check(&document)
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    fn reference(name: &str) -> Value {
        json!({ "$ref": format!("#/components/schemas/{name}") })
    }

    #[test]
    fn api_05_a_property_of_a_response_that_may_be_null_is_required() {
        let schemas = json!({
            "Thing": {
                "required": ["id", "owner"],
                "properties": {
                    "id": { "type": "string" },
                    "owner": { "type": ["string", "null"] },
                    "note": { "type": ["string", "null"] },
                    "part": { "oneOf": [{ "type": "null" }, reference("Part")] },
                    "left_out": { "type": "string" },
                },
            },
            "Part": { "properties": { "size": { "type": ["integer", "null"] } } },
            // A request may leave out a field that may be null.
            "NewThing": { "properties": { "note": { "type": ["string", "null"] } } },
        });

        assert_eq!(
            messages(&reference("Thing"), &schemas),
            [
                "schema Part: the property size may be null, so every response holds it, and it \
                 isn't required (API-05)",
                "schema Thing: the property note may be null, so every response holds it, and it \
                 isn't required (API-05)",
                "schema Thing: the property part may be null, so every response holds it, and it \
                 isn't required (API-05)",
            ]
        );
    }

    #[test]
    fn api_05_a_property_inside_a_response_s_own_schema_or_a_list_s_items_is_read_too() {
        let inline = json!({
            "required": ["items"],
            "properties": { "items": { "type": "array", "items": {
                "allOf": [{ "properties": { "ended_at": { "type": ["string", "null"] } } }],
            } } },
        });

        assert_eq!(
            messages(&inline, &json!({})),
            [
                "GET /v1/things 200.items.items: the property ended_at may be null, so every \
                 response holds it, and it isn't required (API-05)"
            ]
        );
    }

    #[test]
    fn api_05_a_response_whose_nullable_properties_are_required_passes() {
        let schemas = json!({ "Thing": {
            "required": ["note", "part"],
            "properties": {
                "note": { "type": ["string", "null"] },
                "part": { "oneOf": [{ "type": "null" }, { "type": "string" }] },
            },
        } });

        assert_eq!(
            messages(&reference("Thing"), &schemas),
            Vec::<String>::new()
        );
    }

    #[test]
    fn api_05_a_component_with_a_nullable_property_is_a_request_s_or_a_response_s() {
        let schemas = json!({
            "NewThing": { "properties": { "note": { "type": ["string", "null"] } } },
        });

        assert_eq!(
            messages(&reference("NewThing"), &schemas),
            [
                "schema NewThing has a property that may be null and isn't required, and both a \
                 request and a response refer to it: a request may leave the field out, and a \
                 response holds it, so each takes a type of its own (API-05)"
            ]
        );
    }

    #[test]
    fn api_05_a_component_of_a_request_and_a_response_passes_when_its_nullable_ones_are_required() {
        let schemas = json!({ "NewThing": {
            "required": ["note"],
            "properties": { "note": { "type": ["string", "null"] } },
        } });

        assert_eq!(
            messages(&reference("NewThing"), &schemas),
            Vec::<String>::new()
        );
    }

    #[test]
    fn api_05_a_component_that_a_request_s_or_a_response_s_component_refers_to_is_read_as_theirs() {
        let nullable = json!({ "properties": { "note": { "type": ["string", "null"] } } });
        let schemas = json!({
            "Thing": { "required": ["shared"], "properties": { "shared": reference("Shared") } },
            "NewThing": { "properties": {
                "part": reference("NewPart"),
                "shared": reference("Shared"),
            } },
            "NewPart": nullable,
            "Shared": nullable,
        });

        assert_eq!(
            messages(&reference("Thing"), &schemas),
            [
                "schema Shared has a property that may be null and isn't required, and both a \
                 request and a response refer to it: a request may leave the field out, and a \
                 response holds it, so each takes a type of its own (API-05)"
            ]
        );
    }

    #[test]
    fn api_05_a_text_that_names_a_component_outside_a_ref_refers_to_nothing() {
        let schemas = json!({
            "Thing": { "properties": { "note": { "type": ["string", "null"] } } },
            "NewThing": {
                "description": "#/components/schemas/Thing",
                "example": "#/components/schemas/Thing",
            },
        });

        assert_eq!(
            messages(&reference("Thing"), &schemas),
            [
                "schema Thing: the property note may be null, so every response holds it, and it \
                 isn't required (API-05)"
            ]
        );
    }

    #[test]
    fn api_05_a_component_that_nothing_refers_to_is_read_as_a_response_s() {
        let schemas = json!({
            "Item": { "properties": { "ended_at": { "type": ["string", "null"] } } },
        });

        assert_eq!(
            messages(&json!({}), &schemas),
            [
                "schema Item: the property ended_at may be null, so every response holds it, \
                 and it isn't required (API-05)"
            ]
        );
    }
}
