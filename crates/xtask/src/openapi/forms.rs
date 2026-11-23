//! `check-openapi`, for the forms of OpenAPI that the check doesn't read (API-17). The parts of the
//! check that read operations, with their access levels, statuses and codes, read those of the
//! path items of `paths` alone, each under a method that [`METHODS`] lists, with each request body
//! and each response where it stands. So the check refuses:
//! - `webhooks`, `components/pathItems` and `components/callbacks`, each where the document holds
//!   it as anything but an object without members;
//! - a key of a path item of `paths` other than a method of [`METHODS`], `summary`,
//!   `description`, `servers`, `parameters` and an extension, which starts with `x-`, such as
//!   `$ref`;
//! - of an operation there, its `callbacks` on the same terms as `webhooks`, a `requestBody` that
//!   holds `$ref`, and a response that holds `$ref`.
//!
//! Of a document that holds one of these, the check reads nothing else, but what the breaking diff
//! compares with the last final release (`breaking`).

use serde_json::Value;

use super::{METHODS, Problem, quoted};

/// The keys of a path item that hold no operation.
const DESCRIBING: [&str; 4] = ["summary", "description", "servers", "parameters"];

/// A finding for each form of `document` that the module's comment lists.
pub fn check(document: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut unread = |needle: &str, what: String| {
        problems.push((
            quoted(needle),
            format!("{what}, which the check doesn't read (API-17)"),
        ));
    };
    for (holder, needle, whose) in [
        (document.get("webhooks"), "webhooks", "webhooks"),
        (
            document["components"].get("pathItems"),
            "pathItems",
            "components/pathItems",
        ),
        (
            document["components"].get("callbacks"),
            "callbacks",
            "components/callbacks",
        ),
    ] {
        if holds(holder) {
            unread(needle, format!("the document holds `{whose}`"));
        }
    }
    for (path, item) in members(&document["paths"]) {
        for (key, _) in members(item) {
            let read = METHODS.contains(&key.as_str())
                || DESCRIBING.contains(&key.as_str())
                || key.starts_with("x-");
            if !read {
                unread(path, format!("the path item {path} holds `{key}`"));
            }
        }
        for method in METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let here = format!("{} {path}", method.to_ascii_uppercase());
            if holds(operation.get("callbacks")) {
                unread(path, format!("{here} holds `callbacks`"));
            }
            if operation["requestBody"].get("$ref").is_some() {
                unread(path, format!("{here} takes a body that a reference gives"));
            }
            for (status, response) in members(&operation["responses"]) {
                if response.get("$ref").is_some() {
                    unread(path, format!("{here} answers {status} with a reference"));
                }
            }
        }
    }
    problems
}

/// Whether `value` is there, and is anything but an object without members.
fn holds(value: Option<&Value>) -> bool {
    value.is_some_and(|value| value.as_object().is_none_or(|members| !members.is_empty()))
}

/// The members of `value`, an object, or none.
fn members(value: &Value) -> impl Iterator<Item = (&String, &Value)> {
    value.as_object().into_iter().flatten()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{openapi, repo::Repo};

    #[test]
    fn api_17_a_document_with_a_form_that_the_check_doesn_t_read_gets_its_refusal_alone() {
        let document = json!({
            "openapi": "3.1.0",
            "paths": {
                "/v1/things": {
                    "summary": "Things.",
                    "x-group": "things",
                    "post": {
                        "x-access": "public",
                        "requestBody": { "$ref": "#/components/requestBodies/Thing" },
                        "responses": { "404": { "$ref": "#/components/responses/NotFound" } },
                        "callbacks": { "made": { "{$request.body#/url}": {} } },
                    },
                },
                "/v1/others": { "$ref": "#/components/pathItems/Others" },
                "/v1/queried": { "query": { "responses": {} } },
                "/v1/listed": { "get": { "responses": {}, "callbacks": [] } },
                "/v1/plain": { "get": { "responses": {}, "callbacks": {} } },
            },
            "webhooks": { "thing_made": { "post": { "responses": {} } } },
            "components": {
                "pathItems": { "Others": { "get": { "responses": {} } } },
                "callbacks": { "Made": { "{$request.body#/url}": {} } },
                "requestBodies": { "Thing": { "content": {} } },
                "responses": { "NotFound": { "description": "No such thing." } },
            },
        });
        let repo = Repo::of(&[("openapi.json", &document.to_string())]);

        let found: Vec<String> = openapi::check(&repo)
            .findings
            .into_iter()
            .map(|finding| finding.message)
            .collect();

        let unread = |what: &str| format!("{what}, which the check doesn't read (API-17)");
        assert_eq!(
            found,
            [
                unread("the document holds `webhooks`"),
                unread("the document holds `components/pathItems`"),
                unread("the document holds `components/callbacks`"),
                unread("GET /v1/listed holds `callbacks`"),
                unread("the path item /v1/others holds `$ref`"),
                unread("the path item /v1/queried holds `query`"),
                unread("POST /v1/things holds `callbacks`"),
                unread("POST /v1/things takes a body that a reference gives"),
                unread("POST /v1/things answers 404 with a reference"),
            ]
        );
    }
}
