//! `check-openapi`, for lists (API-05, API-06): a success body with `items` is a list, which holds
//! `items`, an array, and `next_cursor`, a string or `null`, both always present, and nothing
//! else, of an operation that takes `limit` and `cursor`; an operation that takes `cursor`
//! answers a list; and no success body is a bare array.

use serde_json::{Map, Value};

use super::{Problem, operations::parameters, quoted, schemas::resolve};

/// The members of a list's body (API-05).
const MEMBERS: [&str; 2] = ["items", "next_cursor"];

/// Every way the document's operations break the shape of a list.
pub fn check(document: &Value) -> Vec<Problem> {
    let empty = Map::new();
    let mut found = Vec::new();
    for (path, item) in document["paths"].as_object().unwrap_or(&empty) {
        for method in super::METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let here = format!("{} {path}", method.to_ascii_uppercase());
            let queries: Vec<&str> = parameters(document, operation, &item["parameters"])
                .into_iter()
                .filter(|parameter| parameter["in"] == "query")
                .filter_map(|parameter| parameter["name"].as_str())
                .collect();
            let mut answers_list = false;
            for (status, response) in operation["responses"].as_object().unwrap_or(&empty) {
                if !status.starts_with('2') {
                    continue;
                }
                let response = resolve(document, response);
                let Some(schema) = response["content"]["application/json"].get("schema") else {
                    continue;
                };
                let schema = resolve(document, schema);
                let messages = shape(document, schema, &queries, &here);
                answers_list |= schema["properties"].get("items").is_some();
                found.extend(messages.into_iter().map(|message| (quoted(path), message)));
            }
            if queries.contains(&"cursor") && !answers_list {
                found.push((
                    quoted(path),
                    format!("{here} takes `cursor` but answers no list (API-05, API-06)"),
                ));
            }
        }
    }
    found
}

/// How a success body's `schema` breaks the shape of a list, for an operation whose query
/// parameters are `queries`.
fn shape(document: &Value, schema: &Value, queries: &[&str], here: &str) -> Vec<String> {
    let mut messages = Vec::new();
    if schema["type"] == "array" {
        messages.push(format!(
            "{here} answers a bare array: a list is `{{items, next_cursor}}` (API-05)"
        ));
        return messages;
    }
    let Some(properties) = schema["properties"].as_object() else {
        return messages;
    };
    if !properties.contains_key("items") {
        return messages;
    }
    let names: Vec<&str> = properties.keys().map(String::as_str).collect();
    if names != MEMBERS {
        messages.push(format!(
            "{here}'s list holds {}, not exactly `items` and `next_cursor` (API-05)",
            names.join(", ")
        ));
    }
    let required: Vec<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !MEMBERS.iter().all(|member| required.contains(member)) {
        messages.push(format!(
            "{here}'s list doesn't require both `items` and `next_cursor` (API-05)"
        ));
    }
    if resolve(document, &properties["items"])["type"] != "array" {
        messages.push(format!("{here}'s `items` isn't an array (API-05)"));
    }
    if let Some(cursor) = properties.get("next_cursor") {
        let types = &resolve(document, cursor)["type"];
        let nullable_string = types.as_array().is_some_and(|types| {
            types.contains(&Value::from("string")) && types.contains(&Value::from("null"))
        });
        if !nullable_string {
            messages.push(format!(
                "{here}'s `next_cursor` isn't a string or `null` (API-05)"
            ));
        }
    }
    for parameter in ["limit", "cursor"] {
        if !queries.contains(&parameter) {
            messages.push(format!(
                "{here} answers a list without taking `{parameter}` (API-06)"
            ));
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn messages(schema: &Value, parameters: &[&str]) -> Vec<String> {
        let parameters: Vec<Value> = parameters
            .iter()
            .map(|name| json!({ "name": name, "in": "query" }))
            .collect();
        let document = json!({ "paths": { "/v1/things": { "get": {
            "parameters": parameters,
            "responses": { "200": { "description": "x",
                "content": { "application/json": { "schema": schema } } } },
        } } } });
        check(&document)
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    fn list() -> Value {
        json!({
            "type": "object",
            "required": ["items", "next_cursor"],
            "properties": {
                "items": { "type": "array", "items": {} },
                "next_cursor": { "type": ["string", "null"] },
            },
        })
    }

    #[test]
    fn api_05_a_list_with_its_paging_passes() {
        assert!(messages(&list(), &["limit", "cursor", "search"]).is_empty());
    }

    #[test]
    fn api_05_a_list_s_shape_and_its_paging_are_checked() {
        let mut wrong = list();
        wrong["properties"]["total"] = json!({ "type": "integer" });
        wrong["required"] = json!(["items"]);
        wrong["properties"]["next_cursor"] = json!({ "type": "string" });
        assert_eq!(
            messages(&wrong, &["limit"]),
            [
                "GET /v1/things's list holds items, next_cursor, total, not exactly `items` and \
                 `next_cursor` (API-05)",
                "GET /v1/things's list doesn't require both `items` and `next_cursor` (API-05)",
                "GET /v1/things's `next_cursor` isn't a string or `null` (API-05)",
                "GET /v1/things answers a list without taking `cursor` (API-06)",
            ]
        );
    }

    #[test]
    fn api_05_a_bare_array_and_a_cursor_without_a_list_are_found() {
        let array = json!({ "type": "array", "items": {} });
        assert_eq!(
            messages(&array, &["cursor"]),
            [
                "GET /v1/things answers a bare array: a list is `{items, next_cursor}` (API-05)",
                "GET /v1/things takes `cursor` but answers no list (API-05, API-06)",
            ]
        );
    }
}
