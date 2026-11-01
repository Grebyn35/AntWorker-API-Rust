//! Checks every response the in-process API gives an integration test against the OpenAPI
//! document of the routes that answered it (API-17). A test fails on a response that breaks the
//! document, even where its own assertions don't look.

use http::{HeaderMap, Method, StatusCode, header};
use serde_json::{Value, json};

/// The checks against one OpenAPI document.
pub struct Contract {
    document: Value,
    /// The document's components, with every object schema closed to properties it doesn't
    /// list. The published document leaves response objects open, because clients ignore fields
    /// they don't know (API-05), but a field the API sends must be documented.
    components: Value,
}

impl Contract {
    /// The document the checks are against.
    pub fn document(&self) -> &Value {
        &self.document
    }

    pub fn new(document: Value) -> Self {
        let mut components = document["components"].clone();
        if let Some(schemas) = components["schemas"].as_object_mut() {
            schemas.values_mut().for_each(close);
        }
        Self {
            document,
            components,
        }
    }

    /// Panics with every way the response breaks the document: a status, content type or field
    /// it doesn't list, and a header it lists that the response lacks. A request that no
    /// operation documents must get the router's `NotFound` or `MethodNotAllowed`. The health
    /// probes aren't in the document, so they aren't checked (OPS-05).
    pub fn check(
        &self,
        method: &Method,
        path: &str,
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
    ) {
        // OpenAPI describes neither the health probes (OPS-05) nor a CORS preflight, which the
        // CORS layer answers before any route (PLATFORM-052).
        let preflight =
            method == Method::OPTIONS && headers.contains_key(header::ACCESS_CONTROL_ALLOW_METHODS);
        if path.starts_with("/health/") || preflight {
            return;
        }
        let path = path.split('?').next().unwrap_or_default();
        let fallbacks = &self.document["components"]["responses"];
        let response = match self.operation(method, path) {
            (_, Some(operation)) => {
                let response = &operation["responses"][status.as_str()];
                assert!(
                    !response.is_null(),
                    "{method} {path} answered {status}, which its documentation doesn't list"
                );
                response
            }
            (Some(item), None) => {
                assert_eq!(
                    status,
                    StatusCode::METHOD_NOT_ALLOWED,
                    "{method} {path} isn't documented, so it must answer 405"
                );
                let allowed = headers[header::ALLOW]
                    .to_str()
                    .unwrap()
                    .to_ascii_lowercase();
                for documented in item.as_object().unwrap().keys() {
                    if METHODS.contains(&documented.as_str()) {
                        assert!(
                            allowed.contains(documented.as_str()),
                            "Allow lacks {documented}"
                        );
                    }
                }
                &fallbacks["MethodNotAllowed"]
            }
            (None, None) => {
                assert_eq!(
                    status,
                    StatusCode::NOT_FOUND,
                    "no documented path matches {path}, so it must answer 404"
                );
                &fallbacks["NotFound"]
            }
        };
        let breaches = self.breaches(method, response, headers, body);
        assert!(
            breaches.is_empty(),
            "{method} {path} answered {status}, which breaks its documentation: {breaches:#?}"
        );
    }

    /// Every way the response breaks `documented`. A `HEAD` response has no body to check.
    fn breaches(
        &self,
        method: &Method,
        documented: &Value,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Vec<String> {
        let mut breaches = Vec::new();
        for name in documented["headers"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|h| h.0)
        {
            if !headers.contains_key(name.as_str()) {
                breaches.push(format!("the documented header {name} is missing"));
            }
        }
        match documented["content"].as_object() {
            None if !body.is_empty() => breaches.push("a body where none is documented".into()),
            None => {}
            Some(content) => {
                let media_type = headers
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.split(';').next())
                    .unwrap_or_default()
                    .trim();
                match content.get(media_type) {
                    None => breaches.push(format!(
                        "the content type {media_type:?} isn't documented, only {:?}",
                        content.keys().collect::<Vec<_>>()
                    )),
                    Some(_) if method == Method::HEAD => {}
                    Some(media) => breaches.extend(self.validate(&media["schema"], body)),
                }
            }
        }
        breaches
    }

    /// The path item whose template matches `path`, and its operation for `method`. A `HEAD`
    /// request is documented by its path's `GET`.
    fn operation(&self, method: &Method, path: &str) -> (Option<&Value>, Option<&Value>) {
        let segments: Vec<&str> = path.split('/').collect();
        let item = self.document["paths"]
            .as_object()
            .into_iter()
            .flatten()
            .find(|(template, _)| {
                let parts: Vec<&str> = template.split('/').collect();
                parts.len() == segments.len()
                    && parts.iter().zip(&segments).all(|(part, segment)| {
                        (part.starts_with('{') && part.ends_with('}')) || part == segment
                    })
            })
            .map(|(_, item)| item);
        let method = if method == Method::HEAD {
            "get".to_owned()
        } else {
            method.as_str().to_ascii_lowercase()
        };
        (item, item.and_then(|item| item.get(method)))
    }

    /// Every way `body` breaks `schema`, with the schema's references resolved against the
    /// document's closed components.
    fn validate(&self, schema: &Value, body: &[u8]) -> Vec<String> {
        let Ok(instance) = serde_json::from_slice::<Value>(body) else {
            return vec!["the body isn't JSON".into()];
        };
        let root = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "components": self.components,
            "allOf": [schema],
        });
        let validator = jsonschema::options()
            .should_validate_formats(true)
            .build(&root)
            .expect("a schema the document holds");
        validator
            .iter_errors(&instance)
            .map(|error| format!("{error}, at {}", error.instance_path()))
            .collect()
    }
}

/// The keys of a path item that hold operations.
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Closes `schema`, and every schema inside it, to properties it doesn't list. The parts of an
/// `allOf` stay open, because each lists only some of the properties the whole allows.
fn close(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    let is_object = match object.get("type") {
        Some(Value::String(kind)) => kind == "object",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "object"),
        _ => object.contains_key("properties"),
    };
    if is_object && !object.contains_key("additionalProperties") {
        object.insert("additionalProperties".to_owned(), Value::Bool(false));
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        properties.values_mut().for_each(close);
    }
    for key in ["items", "additionalProperties"] {
        if let Some(inner) = object.get_mut(key) {
            close(inner);
        }
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(parts) = object.get_mut(key).and_then(Value::as_array_mut) {
            parts.iter_mut().for_each(close);
        }
    }
}
