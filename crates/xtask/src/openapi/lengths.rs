//! `check-openapi`, for the strings of a request body of JSON (API-17): each has a `maxLength`,
//! unless it is a closed set, an `enum`, whose values bound it. A string is a schema whose `type`
//! is `string` or holds it. The check reads the schemas of the body's values: the body's own
//! schema, what the `$ref` of each of them names, once on a cycle, and inside each of them the
//! schemas of `properties`, `items`, `prefixItems`, `additionalProperties`, `patternProperties`,
//! `unevaluatedItems`, `unevaluatedProperties` and `contains`, which describe the value's members,
//! and those of `then`, `else`, `dependentSchemas`, `allOf`, `anyOf` and `oneOf`, which describe
//! the value itself. It reads no other schema inside a schema: `not`'s and `if`'s describe what a
//! value is tested against, `propertyNames`'s the names of its members, `contentSchema`'s what a
//! string holds, and `$defs`'s what a reference names, which the check follows where one does.

use serde_json::Value;

use super::{
    Problem,
    schemas::{DEPTH, inside, resolve},
};

/// The keywords of [`inside`] whose schemas describe a value of the body, each with what it adds
/// to the JSON Pointer of the value whose schema holds it: `/*` for a member of that value, and
/// nothing for the value itself.
const VALUES: [(&str, &str); 13] = [
    ("items", "/*"),
    ("prefixItems", "/*"),
    ("additionalProperties", "/*"),
    ("patternProperties", "/*"),
    ("unevaluatedItems", "/*"),
    ("unevaluatedProperties", "/*"),
    ("contains", "/*"),
    ("then", ""),
    ("else", ""),
    ("dependentSchemas", ""),
    ("allOf", ""),
    ("anyOf", ""),
    ("oneOf", ""),
];

/// Each string of `schema`, a request body's, which findings name as `owner`, that has neither a
/// `maxLength` nor an `enum`.
pub fn problems(document: &Value, schema: &Value, owner: &str, needle: &str) -> Vec<Problem> {
    let mut lengths = Lengths {
        document,
        owner,
        needle,
        seen: Vec::new(),
        problems: Vec::new(),
    };
    lengths.walk(schema, "");
    lengths.problems
}

/// A walk through the schemas of a request body's values.
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
    /// Checks `schema`, which describes the value at the JSON Pointer `at`, and what its `$ref`
    /// names.
    fn walk(&mut self, schema: &Value, at: &str) {
        // A reference that a cycle repeats is followed once.
        if let Some(target) = schema["$ref"].as_str()
            && !self.seen.iter().any(|seen| seen == target)
            && self.seen.len() < DEPTH
        {
            self.seen.push(target.to_owned());
            self.walk(resolve(self.document, schema), at);
            self.seen.pop();
        }
        let is_string = match &schema["type"] {
            Value::String(kind) => kind == "string",
            Value::Array(kinds) => kinds.iter().any(|kind| kind == "string"),
            _ => false,
        };
        let bounded = ["maxLength", "enum"]
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
        for (key, part) in inside(schema) {
            if let Some((_, member)) = VALUES.iter().find(|(name, _)| *name == key) {
                self.walk(part, &format!("{at}{member}"));
            }
        }
    }
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

    fn document(operation: Value, schemas: Value) -> Value {
        json!({ "paths": { "/v1/things": { "post": operation } }, "components": { "schemas": schemas } })
    }

    #[test]
    fn api_17_every_string_of_a_request_body_has_a_max_length() {
        let schemas = json!({
            "Name": { "type": "string", "maxLength": 100 },
            "Note": { "type": "string" },
            "Delivery": { "type": "string", "enum": ["body", "cookie"] },
            "Request": { "type": "object", "properties": {
                "kind": { "type": "string", "const": "thing" },
                "name": { "$ref": "#/components/schemas/Name" },
                "note": { "oneOf": [{ "$ref": "#/components/schemas/Note" }, { "type": "null" }] },
                "delivery": { "$ref": "#/components/schemas/Delivery" },
                "tags": { "type": "array", "items": { "type": ["string", "null"] } },
                "token": { "type": ["string", "null"], "maxLength": 64 },
                "label": { "$ref": "#/components/schemas/Any", "type": "string" },
            } },
            "Any": {},
        });
        let operation = json!({
            "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Request" } } } },
            "responses": {},
        });
        assert_eq!(
            messages(&document(operation, schemas)),
            [
                "POST /v1/things application/json request: the string at /kind has no maxLength (API-17)",
                "POST /v1/things application/json request: the string at /label has no maxLength (API-17)",
                "POST /v1/things application/json request: the string at /note has no maxLength (API-17)",
                "POST /v1/things application/json request: the string at /tags/* has no maxLength (API-17)",
            ]
        );
    }

    #[test]
    fn api_17_the_strings_of_each_schema_of_a_value_inside_a_schema_have_a_max_length() {
        // Under each keyword of `inside`, a schema of an object whose member `in_<keyword>` is a
        // string without a `maxLength`.
        let string_in = |key: &str| json!({ "type": "object", "properties": { format!("in_{key}"): { "type": "string" } } });
        let ones = [
            "items",
            "additionalProperties",
            "contentSchema",
            "not",
            "if",
            "then",
            "else",
            "contains",
            "propertyNames",
            "unevaluatedItems",
            "unevaluatedProperties",
        ];
        let mut body = json!({ "type": "object" });
        for key in ones {
            body[key] = string_in(key);
        }
        for key in ["$defs", "patternProperties", "dependentSchemas"] {
            body[key] = json!({ "a": string_in(key) });
        }
        for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
            body[key] = json!([string_in(key)]);
        }

        let mut found = problems(&json!({}), &body, "the request", "");
        found.sort();

        let members = [
            "items",
            "prefixItems",
            "additionalProperties",
            "patternProperties",
            "unevaluatedItems",
            "unevaluatedProperties",
            "contains",
        ];
        let values = [
            "then",
            "else",
            "dependentSchemas",
            "allOf",
            "anyOf",
            "oneOf",
        ];
        let at = members
            .iter()
            .map(|key| format!("/*/in_{key}"))
            .chain(values.iter().map(|key| format!("/in_{key}")));
        let mut expected: Vec<Problem> = at
            .map(|at| {
                let message = format!("the request: the string at {at} has no maxLength (API-17)");
                (String::new(), message)
            })
            .collect();
        expected.sort();
        assert_eq!(found, expected);
    }
}
