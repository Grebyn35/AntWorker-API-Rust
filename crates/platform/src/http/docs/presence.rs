//! Which fields a response always holds (API-05). A field that a response may leave out is
//! documented as one that is never `null`, so a field that may be `null` is one that every
//! response holds, as its value or as `null`. The document requires each such field, which
//! `utoipa` doesn't of a field whose type is an `Option`.

use std::collections::BTreeSet;

use serde_json::Value;
use utoipa::openapi::{OpenApi, RefOr, Schema, schema::ArrayItems};

/// The start of a reference to a component that is a schema.
const SCHEMAS: &str = "#/components/schemas/";

/// Requires every property that may be `null` of the schemas of `document` that no request
/// gives: the schemas in the responses themselves, and each component but those that a request's
/// body or its parameters refer to, through any number of others, since a request may leave out
/// a field that may be `null`.
pub(in crate::http) fn require_nullable(document: &mut OpenApi) {
    let asked = asked(document);
    for item in document.paths.paths.values_mut() {
        for (_, operation) in super::operations(item) {
            for response in operation.responses.responses.values_mut() {
                let RefOr::T(response) = response else {
                    continue;
                };
                for content in response.content.values_mut() {
                    if let RefOr::T(content) = content
                        && let Some(schema) = &mut content.schema
                    {
                        require(schema);
                    }
                }
            }
        }
    }
    let Some(components) = &mut document.components else {
        return;
    };
    for (name, schema) in &mut components.schemas {
        if !asked.contains(name) {
            require(schema);
        }
    }
}

/// The names of the components that a request's body or its parameters refer to, in `document`,
/// or that such a component refers to.
fn asked(document: &OpenApi) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let Ok(document) = serde_json::to_value(document) else {
        return found;
    };
    let mut pending = Vec::new();
    for item in document["paths"].as_object().into_iter().flatten() {
        references(&item.1["parameters"], &mut pending);
        for operation in item.1.as_object().into_iter().flatten() {
            references(&operation.1["requestBody"], &mut pending);
            references(&operation.1["parameters"], &mut pending);
        }
    }
    while let Some(name) = pending.pop() {
        if found.insert(name.clone()) {
            references(&document["components"]["schemas"][&name], &mut pending);
        }
    }
    found
}

/// Adds to `names` the name of each component that a `$ref` anywhere in `value` names.
fn references(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if let Some(name) = text.strip_prefix(SCHEMAS) {
                names.push(name.to_owned());
            }
        }
        Value::Array(items) => items.iter().for_each(|item| references(item, names)),
        Value::Object(members) => members
            .values()
            .for_each(|member| references(member, names)),
        _ => {}
    }
}

/// Requires every property of `schema` that may be `null`, and those of the schemas it holds.
fn require(schema: &mut RefOr<Schema>) {
    let RefOr::T(schema) = schema else {
        return;
    };
    match schema {
        Schema::Object(object) => {
            for (name, property) in &mut object.properties {
                if nullable(property) && !object.required.contains(name) {
                    object.required.push(name.clone());
                }
                require(property);
            }
        }
        Schema::Array(array) => {
            if let ArrayItems::RefOrSchema(items) = &mut array.items {
                require(items);
            }
        }
        Schema::OneOf(one_of) => one_of.items.iter_mut().for_each(require),
        Schema::AllOf(all_of) => all_of.items.iter_mut().for_each(require),
        Schema::AnyOf(any_of) => any_of.items.iter_mut().for_each(require),
        _ => {}
    }
}

/// Whether `schema` takes `null`: its `type` holds it, or one of its `oneOf` is it.
fn nullable(schema: &RefOr<Schema>) -> bool {
    let Ok(schema) = serde_json::to_value(schema) else {
        return false;
    };
    let null = |schema: &Value| match &schema["type"] {
        Value::String(kind) => kind == "null",
        Value::Array(kinds) => kinds.iter().any(|kind| kind == "null"),
        _ => false,
    };
    null(&schema) || schema["oneOf"].as_array().into_iter().flatten().any(null)
}
