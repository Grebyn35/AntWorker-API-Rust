//! Checks every response the in-process API gives an integration test against the OpenAPI
//! document of the routes that answered it, every body a test sends through a typed call
//! against the request body the document gives the operation, and every frame of a stream that a
//! test reads against the event catalog (API-17, REALTIME-008). A test fails on a response, a
//! typed request or a frame that breaks the document, even where its own assertions don't look.

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

    /// Panics with every way the typed request body breaks the `requestBody` the document gives
    /// the operation for `method` and `path`, in the content for the request's media type, or
    /// if the operation documents no request body. A request to a path or method the document
    /// doesn't hold isn't checked, because the response check requires the router's 404 or 405.
    pub fn check_request(&self, method: &Method, path: &str, headers: &HeaderMap, body: &[u8]) {
        let path = path.split('?').next().unwrap_or_default();
        let (_, Some(operation)) = self.operation(method, path) else {
            return;
        };
        let request_body = &operation["requestBody"];
        assert!(
            !request_body.is_null(),
            "{method} {path} was sent a typed body, but its documentation lists no request body"
        );
        let media_type = headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .unwrap_or_default()
            .trim();
        let breaches = match request_body["content"].get(media_type) {
            None => vec![format!(
                "the content type {media_type:?} isn't documented, only {:?}",
                request_body["content"]
                    .as_object()
                    .map(|content| content.keys().collect::<Vec<_>>())
            )],
            // A file's bytes are no JSON that a schema describes.
            Some(_) if !is_json(media_type) => Vec::new(),
            Some(media) => self.validate(&media["schema"], body),
        };
        assert!(
            breaches.is_empty(),
            "{method} {path} was sent a body that breaks its documentation: {breaches:#?}"
        );
    }

    /// Panics with every way a frame of the stream, its `event`, `id` and `data` as a client's
    /// parser reads them, breaks the entry of the event catalog that its `event` names, its
    /// `data` read as JSON against the entry's `contentSchema` (REALTIME-008).
    pub fn check_frame(&self, event: Option<&str>, id: Option<&str>, data: Option<&str>) {
        let entries = self.components["schemas"]["Frame"]["oneOf"]
            .as_array()
            .expect("the document's event catalog");
        let component = entries
            .iter()
            .filter_map(|entry| entry["$ref"].as_str())
            .filter_map(|reference| reference.rsplit('/').next())
            .find(|component| {
                self.components["schemas"][*component]["properties"]["event"]["enum"][0].as_str()
                    == event
            });
        assert!(
            component.is_some(),
            "the stream sent {event:?}, which the event catalog doesn't list"
        );
        let component = component.unwrap();
        let frame = &self.components["schemas"][component];
        let mut instance = json!({});
        for (field, value) in [("event", event), ("id", id), ("data", data)] {
            if let Some(value) = value {
                instance[field] = Value::from(value);
            }
        }
        let mut breaches = self.validate(
            &json!({ "$ref": format!("#/components/schemas/{component}") }),
            instance.to_string().as_bytes(),
        );
        if let Some(data) = data {
            breaches.extend(self.validate(
                &frame["properties"]["data"]["contentSchema"],
                data.as_bytes(),
            ));
        }
        assert!(
            breaches.is_empty(),
            "the stream sent a {event:?} frame that breaks the event catalog: {breaches:#?}"
        );
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
        // OpenAPI describes neither the health probes (OPS-05), nor the API reference that
        // development serves (PLATFORM-052), nor a CORS preflight, which the CORS layer answers
        // before any route (PLATFORM-058).
        let preflight =
            method == Method::OPTIONS && headers.contains_key(header::ACCESS_CONTROL_ALLOW_METHODS);
        let path = path.split('?').next().unwrap_or_default();
        let reference = path == "/docs" || path.starts_with("/docs/");
        if path.starts_with("/health/") || reference || preflight {
            return;
        }
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
                    // A stream's frames, and a file's bytes, are no JSON that a schema
                    // describes: their tests read them.
                    Some(_) if method == Method::HEAD || !is_json(media_type) => {}
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
    for key in ["items", "additionalProperties", "contentSchema"] {
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

/// Whether `media_type` is JSON, as a problem's is, whose body a schema describes.
fn is_json(media_type: &str) -> bool {
    media_type == "application/json" || media_type.ends_with("+json")
}
