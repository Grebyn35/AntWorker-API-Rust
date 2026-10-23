//! Checks every response an integration test receives against the OpenAPI document of the
//! routes that answered it (API-17). A test fails on a response that breaks the document, even
//! where its own assertions don't look.

use http::{HeaderMap, Method, StatusCode, header};
use serde_json::{Value, json};

/// The checks against one OpenAPI document.
pub struct Contract {
    document: Value,
    /// The document's component schemas, with every object closed to properties it doesn't
    /// list. The published document leaves response objects open, because clients ignore
    /// fields they don't know (API-05), but a field the API sends must be documented.
    components: Value,
}

impl Contract {
    pub fn new(document: Value) -> Self {
        let mut components = document["components"].clone();
        if let Some(schemas) = components["schemas"].as_object_mut() {
            for schema in schemas.values_mut() {
                if schema["type"] == "object" && schema.get("additionalProperties").is_none() {
                    schema["additionalProperties"] = Value::Bool(false);
                }
            }
        }
        Self {
            document,
            components,
        }
    }

    /// Panics with every way the response breaks the document. The health probes aren't in the
    /// document, so they aren't checked (OPS-05).
    pub fn check(
        &self,
        method: &Method,
        path: &str,
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
    ) {
        if path.starts_with("/health/") {
            return;
        }
        let path = path.split('?').next().unwrap_or_default();
        let (item, operation) = self.operation(method, path);
        let responses = match (item, operation) {
            (_, Some(operation)) => &operation["responses"],
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
                let methods = [
                    "get", "put", "post", "delete", "options", "head", "patch", "trace",
                ];
                for documented in item.as_object().unwrap().keys() {
                    if methods.contains(&documented.as_str()) {
                        assert!(
                            allowed.contains(documented.as_str()),
                            "Allow lacks {documented}"
                        );
                    }
                }
                return self.check_problem(body, "request.method_not_allowed");
            }
            (None, None) => {
                assert_eq!(
                    status,
                    StatusCode::NOT_FOUND,
                    "no documented path matches {path}, so it must answer 404"
                );
                return self.check_problem(body, "request.not_found");
            }
        };
        let response = responses.get(status.as_str());
        assert!(
            response.is_some(),
            "{method} {path} answered {status}, which its documentation doesn't list"
        );
        let response = response.unwrap();
        let mut breaches = Vec::new();
        for name in response["headers"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|h| h.0)
        {
            if !headers.contains_key(name.as_str()) {
                breaches.push(format!("the documented header {name} is missing"));
            }
        }
        match response["content"].as_object() {
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
                    Some(media) => breaches.extend(self.validate(&media["schema"], body)),
                }
            }
        }
        assert!(
            breaches.is_empty(),
            "{method} {path} answered {status}, which breaks its documentation: {breaches:#?}"
        );
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
    /// document's components.
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

    fn check_problem(&self, body: &[u8], code: &str) {
        let breaches = self.validate(&json!({"$ref": "#/components/schemas/Problem"}), body);
        assert!(breaches.is_empty(), "not a problem: {breaches:#?}");
        let problem: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(problem["code"], code);
    }
}
