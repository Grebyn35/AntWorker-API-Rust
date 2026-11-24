//! `check-openapi`, for the schemas: properties' names are `snake_case` (API-05), IDs are
//! `format: uuid`, timestamps `format: date-time`, and closed sets `snake_case` strings (API-11).
//! Of the places that it reads, it gives the schema of each request body of JSON,
//! `application/json` or a type that ends in `+json`, to [`lengths`], and each header to
//! [`headers`] (API-17).
//!
//! An ID is a property or a parameter named `id` or `<name>_id`, but `trace_id`, `device_id`
//! and `client_id`, the IDs that API-11 excepts, and a timestamp a property named `<event>_at`,
//! as DATA-01 names their columns.
//!
//! The check reads the schema of each place of the document that holds one, where it stands: a
//! member of `components/schemas`, the `schema` of a parameter and of a header, and the `schema`
//! and the `x-itemSchema` of each content type of the `content` of a parameter, a header, a
//! request body and a response, a problem's for its properties' names alone, with the headers of
//! the content type's `encoding`. It finds those in the members of `components/parameters`,
//! `requestBodies`, `responses`, `headers`, `pathItems` and `callbacks`, and in the path items of
//! `paths` and `webhooks`: a path item's parameters and operations, an operation's parameters,
//! request body, responses and callbacks, a response's headers, and a callback's path items. A
//! reference at one of those places isn't followed: it names a component of the place's kind,
//! `#/components/<kind>/<name>`, which the check reads where it stands, or the check refuses it.
//! It reads the IDs among the parameters of each operation there, with the names in the template
//! of an operation's path of `paths`, and, inside a schema, the schemas that [`formats`] names. Of
//! a document that [`forms`](super::forms) refuses, the check reads none of these.

use serde_json::Value;

use super::{METHODS, Problem, headers, is_snake_case, lengths, operations::parameters, quoted};

/// The IDs that API-11 excepts from being UUIDs: a trace ID, the 32 lowercase hexadecimal digits
/// of W3C Trace Context, a device ID, which its client chooses, and an OAuth provider's client
/// ID, which the provider issues.
const NOT_UUIDS: [&str; 3] = ["trace_id", "device_id", "client_id"];

/// The most references a schema follows before the check gives up on a cycle.
pub const DEPTH: usize = 16;

/// The keywords of JSON Schema whose value is one schema.
const INSIDE: [&str; 11] = [
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

/// The keywords of JSON Schema whose value is an object of schemas, but `properties`.
const NAMED: [&str; 3] = ["$defs", "patternProperties", "dependentSchemas"];

/// The keywords of JSON Schema whose value is an array of schemas.
const LISTED: [&str; 4] = ["allOf", "anyOf", "oneOf", "prefixItems"];

/// Each schema inside `schema` but those of its `properties`, with the keyword that holds it: the
/// value of each keyword of [`INSIDE`] that is an object, each member of the value of each of
/// [`NAMED`], and each item of the value of each of [`LISTED`].
pub fn inside(schema: &Value) -> impl Iterator<Item = (&'static str, &Value)> {
    let one = INSIDE
        .into_iter()
        .filter_map(move |key| Some((key, schema.get(key).filter(|part| part.is_object())?)));
    let named = NAMED
        .into_iter()
        .flat_map(move |key| members(&schema[key]).map(move |(_, part)| (key, part)));
    let listed = LISTED.into_iter().flat_map(move |key| {
        let parts = schema[key].as_array().into_iter().flatten();
        parts.map(move |part| (key, part))
    });
    one.chain(named).chain(listed)
}

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

/// Every way the document's schemas break API-05 and API-11, and its request bodies API-17: those
/// of each place that [`Places`] reads, the IDs among each operation's parameters among them.
pub fn problems(document: &Value) -> Vec<Problem> {
    let mut places = Places {
        document,
        problems: Vec::new(),
    };
    for kind in KINDS {
        for (name, value) in members(&document["components"][kind]) {
            let at = if kind == "schemas" {
                format!("schema {name}")
            } else {
                format!("components/{kind}/{name}")
            };
            places.place(kind, value, &at, &quoted(name));
        }
    }
    for (key, prefix) in [("paths", ""), ("webhooks", "webhook ")] {
        for (name, item) in members(&document[key]) {
            places.place("pathItems", item, &format!("{prefix}{name}"), &quoted(name));
        }
    }
    places.problems
}

/// The kinds of the members of `components` that hold a schema, or a place that does.
const KINDS: [&str; 7] = [
    "schemas",
    "parameters",
    "requestBodies",
    "responses",
    "headers",
    "pathItems",
    "callbacks",
];

/// The places of a document that hold a schema, each read where it stands: a member of a kind of
/// [`KINDS`] of `components`, a path item of `paths` and of `webhooks`, and what [`Places::place`]
/// reads inside each of them.
struct Places<'a> {
    document: &'a Value,
    problems: Vec<Problem>,
}

impl Places<'_> {
    /// Checks `value`, a `kind` of [`KINDS`], which findings name as `at`. A parameter and a
    /// header hold a `schema` and a `content`, a request body and a response a `content`, a
    /// response its `headers` too, and a callback a path item under each of its expressions. A
    /// reference here, but a path item's, stands for all of `value`: it isn't followed, and names
    /// a component of `kind`, which is read where it stands, or is refused.
    fn place(&mut self, kind: &str, value: &Value, at: &str, needle: &str) {
        if kind != "schemas"
            && let Some(target) = value["$ref"].as_str()
        {
            if !names_component(target, kind) {
                self.problems.push(refused(at, target, kind));
            }
            if kind != "pathItems" {
                return;
            }
        }
        match kind {
            "schemas" => self.schema(value, at, needle),
            "parameters" | "headers" => {
                if kind == "headers" || value["in"] == "header" {
                    let refused = headers::problems(self.document, value, at, needle);
                    self.problems.extend(refused);
                }
                self.schema(&value["schema"], at, needle);
                self.content(&value["content"], at, needle, false);
            }
            "requestBodies" => self.content(&value["content"], at, needle, true),
            "responses" => {
                self.content(&value["content"], at, needle, false);
                for (name, header) in members(&value["headers"]) {
                    self.place("headers", header, &format!("{at} header {name}"), needle);
                }
            }
            "callbacks" => {
                // A callback's extensions are no path items.
                let items = members(value).filter(|(key, _)| !key.starts_with("x-"));
                for (expression, item) in items {
                    self.place("pathItems", item, &format!("{expression} of {at}"), needle);
                }
            }
            _ => self.path_item(value, at, needle),
        }
    }

    /// Checks `schema`, which findings name as `at`, with [`formats`] and [`properties`].
    fn schema(&mut self, schema: &Value, at: &str, needle: &str) {
        formats(self.document, schema, at, needle, &mut self.problems);
        properties(at, schema, &mut self.problems);
    }

    /// Checks the parameters of `item`, a path item, and those of each of its operations, with
    /// the IDs among them, the operation's request body, responses and callbacks.
    fn path_item(&mut self, item: &Value, at: &str, needle: &str) {
        // A path's template names parameters too, where `item` is a path item of `paths`.
        let template = if std::ptr::eq(&self.document["paths"][at], item) {
            at
        } else {
            ""
        };
        self.parameters(&item["parameters"], at, needle);
        for method in METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let here = format!("{} {at}", method.to_ascii_uppercase());
            let shared = &item["parameters"];
            let found = identifiers(self.document, template, operation, shared, &here, needle);
            self.problems.extend(found);
            self.parameters(&operation["parameters"], &here, needle);
            if let Some(body) = operation.get("requestBody") {
                self.place("requestBodies", body, &here, needle);
            }
            for (status, response) in members(&operation["responses"]) {
                self.place("responses", response, &format!("{here} {status}"), needle);
            }
            for (name, callback) in members(&operation["callbacks"]) {
                let at = format!("the callback {name} of {here}");
                self.place("callbacks", callback, &at, needle);
            }
        }
    }

    fn parameters(&mut self, parameters: &Value, at: &str, needle: &str) {
        for parameter in parameters.as_array().into_iter().flatten() {
            let name = parameter["name"]
                .as_str()
                .map_or(String::new(), |name| format!(" {name}"));
            self.place(
                "parameters",
                parameter,
                &format!("{at} parameter{name}"),
                needle,
            );
        }
    }

    /// Checks of each content type of `content` its `schema` and its `x-itemSchema`, the schema of
    /// a stream's items, those of a problem's for their properties' names alone, the headers of its
    /// `encoding`, and, in a request body of JSON, that each string has a `maxLength` or an `enum`.
    fn content(&mut self, content: &Value, at: &str, needle: &str, request: bool) {
        for (media, of_media) in members(content) {
            let owner = if request {
                format!("{at} {media} request")
            } else {
                format!("{at} {media}")
            };
            for key in ["schema", "x-itemSchema"] {
                // A problem's codes are ERR-02's, not closed sets of API-11.
                if media == "application/problem+json" {
                    properties(&owner, &of_media[key], &mut self.problems);
                } else {
                    self.schema(&of_media[key], &owner, needle);
                }
            }
            if request && (media == "application/json" || media.ends_with("+json")) {
                let found = lengths::problems(self.document, &of_media["schema"], &owner, needle);
                self.problems.extend(found);
            }
            for (property, encoding) in members(&of_media["encoding"]) {
                for (name, header) in members(&encoding["headers"]) {
                    let at = format!("{owner} {property} header {name}");
                    self.place("headers", header, &at, needle);
                }
            }
        }
    }
}

/// The members of `value`, an object, or none.
fn members(value: &Value) -> impl Iterator<Item = (&String, &Value)> {
    value.as_object().into_iter().flatten()
}

/// Whether `target`, a reference, names a component of `kind`: `#/components/<kind>/<name>`.
fn names_component(target: &str, kind: &str) -> bool {
    target
        .strip_prefix("#/components/")
        .and_then(|rest| rest.strip_prefix(kind))
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|name| !name.is_empty() && !name.contains('/'))
}

/// The finding for `target`, a reference at `at`, where a component of `kind` stands.
fn refused(at: &str, target: &str, kind: &str) -> Problem {
    (
        quoted(target),
        format!(
            "{at}: the reference {target} names no component of #/components/{kind}/, which is \
             where the check reads what a reference here names (API-11)"
        ),
    )
}

/// API-11: the IDs among the names in `template`, an operation's path or none, and among its
/// parameters, but those of [`NOT_UUIDS`], are UUIDs, each of them, of a filter that takes
/// several, an array's item (PLATFORM-088).
fn identifiers(
    document: &Value,
    template: &str,
    operation: &Value,
    shared: &Value,
    here: &str,
    needle: &str,
) -> Vec<Problem> {
    let parameters = parameters(document, operation, shared);
    let mut names: Vec<String> = template
        .split('/')
        .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
        .map(str::to_owned)
        .collect();
    for parameter in &parameters {
        let name = parameter["name"].as_str().unwrap_or_default();
        if !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    }
    names
        .into_iter()
        .filter(|name| is_id(name) && !NOT_UUIDS.contains(&name.as_str()))
        .filter(|name| {
            !parameters.iter().any(|parameter| {
                let schema = resolve(document, &parameter["schema"]);
                let id = if schema["type"] == "array" {
                    &schema["items"]
                } else {
                    &parameter["schema"]
                };
                parameter["name"] == name.as_str() && formats_of(document, id, 0).contains(&"uuid")
            })
        })
        .map(|name| {
            (
                needle.to_owned(),
                format!("{here}: the parameter {name} isn't documented as format: uuid (API-11)"),
            )
        })
        .collect()
}

/// API-11: the IDs and timestamps among `schema`'s properties, and those of the schemas inside
/// it, under `properties` and those that [`inside`] gives, have their formats, and its closed sets
/// are `snake_case`. A reference isn't followed: it names a component of `schemas`, which is
/// checked where it stands, or is refused.
fn formats(
    document: &Value,
    schema: &Value,
    owner: &str,
    needle: &str,
    problems: &mut Vec<Problem>,
) {
    if let Some(target) = schema["$ref"].as_str()
        && !names_component(target, "schemas")
    {
        problems.push(refused(owner, target, "schemas"));
    }
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
            // A frame's type is an event's name, which API-19 shapes, not a closed set of
            // API-11; `events` checks it.
            if name == "event" && !schema["x-event"].is_null() {
                continue;
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
    for (key, part) in inside(schema) {
        // An entry's action is an action's name, which the audit spec shapes, not a closed set of
        // API-11; `actions` checks it.
        if LISTED.contains(&key) && !schema["x-audit-action"].is_null() {
            let mut part = part.clone();
            if let Some(properties) = part["properties"].as_object_mut() {
                properties.remove("action");
            }
            formats(document, &part, owner, needle, problems);
        } else {
            formats(document, part, owner, needle, problems);
        }
    }
}

/// API-05: every property of `schema`, and of the schemas inside it, those of its properties and
/// those that [`inside`] gives, is `snake_case`. A reference isn't followed.
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
    for (_, part) in inside(schema) {
        properties(owner, part, problems);
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

/// Whether `name` names an ID: `id`, or `<name>_id`.
fn is_id(name: &str) -> bool {
    name == "id" || name.ends_with("_id")
}

#[cfg(test)]
pub mod tests {
    use serde_json::json;

    use super::*;

    /// The messages of the problems that the checks of the document's schemas find in
    /// `document`, which the tests of those checks in the other files of the module read too.
    pub fn messages(document: &Value) -> Vec<String> {
        problems(document)
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    /// A document whose `POST /v1/things` is `operation`, beside the components `schemas`.
    pub fn document(operation: Value, schemas: Value) -> Value {
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
                { "name": "maker_id", "in": "query", "schema": {
                    "type": "array", "items": { "type": "string", "format": "uuid" } } },
                { "name": "seller_id", "in": "query", "schema": {
                    "type": "array", "items": { "type": "string" } } },
            ],
            "responses": {},
        } } } });
        assert_eq!(
            messages(&document),
            [
                "GET /v1/things/{thing_id}: the parameter thing_id isn't documented as format: uuid (API-11)",
                "GET /v1/things/{thing_id}: the parameter seller_id isn't documented as format: uuid (API-11)",
            ]
        );
    }

    #[test]
    fn api_11_an_id_parameter_in_a_header_or_a_cookie_is_a_uuid() {
        let document = json!({ "paths": { "/v1/things": { "get": {
            "parameters": [
                { "name": "thing_id", "in": "header", "schema": { "type": "string" } },
                { "name": "owner_id", "in": "cookie", "schema": { "type": "string" } },
                { "name": "maker_id", "in": "header", "schema": { "type": "string", "format": "uuid" } },
            ],
            "responses": {},
        } } } });
        assert_eq!(
            messages(&document),
            [
                "GET /v1/things: the parameter thing_id isn't documented as format: uuid (API-11)",
                "GET /v1/things: the parameter owner_id isn't documented as format: uuid (API-11)",
            ]
        );
    }

    #[test]
    fn api_11_a_parameter_s_closed_set_is_snake_case() {
        let document = json!({ "paths": { "/v1/things": {
            "parameters": [
                { "name": "view", "in": "query", "schema": { "type": "string", "enum": ["full", "Brief"] } },
            ],
            "get": {
                "parameters": [
                    { "name": "kind", "in": "query", "schema": {
                        "type": "string", "enum": ["plain_thing", "BigThing"] } },
                    { "name": "X-Mode", "in": "header", "schema": {
                        "type": "string", "enum": ["fast", "Slow"] } },
                ],
                "responses": {},
            },
        } } });
        assert_eq!(
            messages(&document),
            [
                "/v1/things parameter view: the value Brief of a closed set isn't snake_case (API-11)",
                "GET /v1/things parameter kind: the value BigThing of a closed set isn't snake_case (API-11)",
                "GET /v1/things parameter X-Mode: the value Slow of a closed set isn't snake_case (API-11)",
            ]
        );
    }

    #[test]
    fn api_11_a_parameter_that_api_11_excepts_is_no_uuid() {
        let document = json!({ "paths": { "/v1/users/me/devices/{device_id}": { "get": {
            "parameters": [
                { "name": "device_id", "in": "path", "schema": { "type": "string", "maxLength": 64 } },
                { "name": "trace_id", "in": "query", "schema": { "type": "string" } },
                { "name": "client_id", "in": "query", "schema": { "type": "string" } },
                { "name": "owner_id", "in": "query", "schema": { "type": "string" } },
            ],
            "responses": {},
        } } } });
        assert_eq!(
            messages(&document),
            [
                "GET /v1/users/me/devices/{device_id}: the parameter owner_id isn't documented as format: uuid (API-11)",
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
    fn api_11_an_audit_entry_s_action_is_no_closed_set() {
        let schemas = json!({
            "ThingMadeEntry": {
                "allOf": [{ "type": "object", "properties": {
                    "action": { "type": "string", "enum": ["thing.made"] },
                    "resource_type": { "type": "string", "enum": ["thing", "bigThing"] },
                } }],
                "x-audit-action": "thing.made",
            },
            "Other": { "allOf": [{ "type": "object", "properties": {
                "action": { "type": "string", "enum": ["thing.made"] },
            } }] },
        });
        assert_eq!(
            messages(&document(json!({ "responses": {} }), schemas)),
            [
                "schema Other.action: the value thing.made of a closed set isn't snake_case (API-11)",
                "schema ThingMadeEntry.resource_type: the value bigThing of a closed set isn't snake_case (API-11)",
            ]
        );
    }

    /// A schema with an ID that isn't `format: uuid` and a closed set that isn't `snake_case`.
    fn unformatted() -> Value {
        json!({ "type": "object", "properties": {
            "thing_id": { "type": "string" },
            "kind": { "type": "string", "enum": ["Big"] },
        } })
    }

    /// The findings of [`unformatted`] at each of `owners`.
    fn unformatted_at(owners: &[&str]) -> Vec<String> {
        owners
            .iter()
            .flat_map(|owner| {
                [
                    format!(
                        "{owner}.kind: the value Big of a closed set isn't snake_case (API-11)"
                    ),
                    format!("{owner}: the ID thing_id isn't format: uuid (API-11)"),
                ]
            })
            .collect()
    }

    #[test]
    fn api_11_the_schema_of_each_kind_of_component_is_read() {
        let bad = unformatted();
        let document = json!({ "components": {
            "parameters": { "Kind": { "name": "kind", "in": "query", "schema": bad } },
            "requestBodies": { "Thing": { "content": { "multipart/form-data": { "schema": bad } } } },
            "responses": { "Thing": {
                "description": "x",
                "content": { "application/json": { "schema": bad } },
                "headers": { "X-Thing": { "schema": bad } },
            } },
            "headers": { "X-Kind": { "schema": bad } },
            "pathItems": { "Things": { "get": { "parameters": [
                { "name": "view", "in": "query", "schema": bad },
            ] } } },
            "callbacks": { "Made": { "{$request.body#/url}": { "post": { "responses": {
                "200": { "content": { "application/json": { "schema": bad } } },
            } } } } },
        } });

        assert_eq!(
            messages(&document),
            unformatted_at(&[
                "components/parameters/Kind",
                "components/requestBodies/Thing multipart/form-data request",
                "components/responses/Thing application/json",
                "components/responses/Thing header X-Thing",
                "components/headers/X-Kind",
                "GET components/pathItems/Things parameter view",
                "POST {$request.body#/url} of components/callbacks/Made 200 application/json",
            ])
        );
    }

    #[test]
    fn api_11_the_schema_of_each_place_of_an_operation_is_read() {
        let bad = unformatted();
        let document = json!({ "paths": { "/v1/things": {
            "parameters": [{ "name": "filter", "in": "query", "content": {
                "application/json": { "schema": bad } } }],
            "post": {
                "parameters": [{ "name": "view", "in": "query", "schema": bad }],
                "requestBody": { "content": { "multipart/form-data": {
                    "schema": bad,
                    "encoding": { "file": { "headers": { "X-Part": { "schema": bad } } } },
                } } },
                "responses": { "200": {
                    "description": "x",
                    "content": {
                        "application/json": { "schema": bad },
                        "text/event-stream": { "schema": { "type": "string" }, "x-itemSchema": bad },
                    },
                    "headers": {
                        "X-Thing": { "schema": bad },
                        "X-Other": { "content": { "text/plain": { "schema": bad } } },
                    },
                } },
                "callbacks": { "made": { "{$request.body#/url}": { "post": { "responses": {
                    "200": { "content": { "application/json": { "schema": bad } } },
                } } } } },
            },
        } } });

        assert_eq!(
            messages(&document),
            unformatted_at(&[
                "/v1/things parameter filter application/json",
                "POST /v1/things parameter view",
                "POST /v1/things multipart/form-data request",
                "POST /v1/things multipart/form-data request file header X-Part",
                "POST /v1/things 200 application/json",
                "POST /v1/things 200 text/event-stream",
                "POST /v1/things 200 header X-Other text/plain",
                "POST /v1/things 200 header X-Thing",
                "POST {$request.body#/url} of the callback made of POST /v1/things 200 application/json",
            ])
        );
    }

    #[test]
    fn api_11_the_schema_of_each_place_of_a_webhook_is_read() {
        let document = json!({ "webhooks": { "thing_made": { "post": {
            "parameters": [{ "name": "thing_id", "in": "query", "schema": { "type": "string" } }],
            "requestBody": { "content": { "multipart/form-data": { "schema": unformatted() } } },
        } } } });

        let mut expected = vec![
            "POST webhook thing_made: the parameter thing_id isn't documented as format: uuid \
             (API-11)"
                .to_owned(),
        ];
        expected.extend(unformatted_at(&[
            "POST webhook thing_made multipart/form-data request",
        ]));
        assert_eq!(messages(&document), expected);
    }

    #[test]
    fn api_11_a_reference_names_a_component_of_its_place_s_kind_which_is_read_once() {
        let document = json!({
            "paths": { "/v1/things": { "post": {
                "parameters": [{ "$ref": "#/paths/~1v1~1other/get/parameters/0" }],
                "requestBody": { "$ref": "#/components/requestBodies/Thing" },
                "responses": {
                    "200": { "$ref": "#/components/schemas/Thing" },
                    "201": { "description": "x", "content": { "application/json": {
                        "schema": { "$ref": "#/x-shared/Thing" } } } },
                },
            } } },
            "components": {
                "requestBodies": { "Thing": { "content": { "multipart/form-data": {
                    "schema": unformatted() } } } },
                "schemas": { "Thing": { "type": "object" } },
            },
        });

        let mut expected =
            unformatted_at(&["components/requestBodies/Thing multipart/form-data request"]);
        expected.extend([
            "POST /v1/things parameter: the reference #/paths/~1v1~1other/get/parameters/0 names no \
             component of #/components/parameters/, which is where the check reads what a \
             reference here names (API-11)"
                .to_owned(),
            "POST /v1/things 200: the reference #/components/schemas/Thing names no component of \
             #/components/responses/, which is where the check reads what a reference here names \
             (API-11)"
                .to_owned(),
            "POST /v1/things 201 application/json: the reference #/x-shared/Thing names no \
             component of #/components/schemas/, which is where the check reads what a reference \
             here names (API-11)"
                .to_owned(),
        ]);
        assert_eq!(messages(&document), expected);
    }

    #[test]
    fn api_11_each_schema_inside_a_schema_is_read() {
        let set = |key: &str| json!({ "type": "string", "enum": [format!("In{key}")] });
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
        let mut thing = json!({ "$defs": { "Owner": unformatted() } });
        for key in ones {
            thing[key] = set(key);
        }
        for key in ["patternProperties", "dependentSchemas"] {
            thing[key] = json!({ "a": set(key) });
        }
        thing["prefixItems"] = json!([set("prefixItems")]);
        let document = json!({ "components": { "schemas": { "Thing": thing } } });

        let mut found = messages(&document);
        found.sort();

        let mut expected = unformatted_at(&["schema Thing"]);
        for key in ones
            .iter()
            .chain(&["patternProperties", "dependentSchemas", "prefixItems"])
        {
            expected.push(format!(
                "schema Thing: the value In{key} of a closed set isn't snake_case (API-11)"
            ));
        }
        expected.sort();
        assert_eq!(found, expected);
    }
}
