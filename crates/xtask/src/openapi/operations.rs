//! `check-openapi`, for each operation: the scope in its path (API-02), its method's meaning
//! (API-03), and its statuses (API-04), as far as the document shows them.

use serde_json::{Map, Value};

use super::{Problem, quoted};

/// The methods API-03 gives a meaning.
const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

/// The success statuses API-04 lists.
const SUCCESSES: [&str; 4] = ["200", "201", "202", "204"];

/// The media type of a `PATCH` body (RFC 7396).
const MERGE_PATCH: &str = "application/merge-patch+json";

/// The prefix of every path that acts on one workspace (API-02).
const WORKSPACE: &str = "/v1/workspaces/{workspace_id}";

/// The prefix of the caller's own resources (API-02).
const CALLER: &str = "/v1/users/me";

/// Every way the document's operations break API-02 to API-04.
pub fn check(document: &Value) -> Vec<Problem> {
    let empty = Map::new();
    let mut found = Vec::new();
    for (path, item) in document["paths"].as_object().unwrap_or(&empty) {
        for method in super::METHODS {
            if let Some(operation) = item.get(method) {
                found.extend(problems(
                    document,
                    path,
                    method,
                    operation,
                    &item["parameters"],
                ));
            }
        }
    }
    found
}

/// Every way the operation at `method` and `path` breaks API-02 to API-04, with the path item's
/// parameters, `shared`.
fn problems(
    document: &Value,
    path: &str,
    method: &str,
    operation: &Value,
    shared: &Value,
) -> Vec<Problem> {
    let here = format!("{} {path}", method.to_ascii_uppercase());
    let mut messages = scope(document, path, operation, shared, &here);
    messages.extend(meaning(path, method, operation, &here));
    messages.extend(statuses(method, operation, &here));
    messages
        .into_iter()
        .map(|message| (quoted(path), message))
        .collect()
}

/// API-02: a workspace's resources are under `/v1/workspaces/{workspace_id}/`, the caller's own
/// under `/v1/users/me/`, and no header selects a workspace.
fn scope(
    document: &Value,
    path: &str,
    operation: &Value,
    shared: &Value,
    here: &str,
) -> Vec<String> {
    let mut messages = Vec::new();
    let access = operation["x-access"].as_str().unwrap_or_default();
    let under = |prefix: &str| path == prefix || path.starts_with(&format!("{prefix}/"));
    if access == "workspace" && !under(WORKSPACE) {
        messages.push(format!(
            "{here} acts on a workspace, so its path starts with {WORKSPACE} (API-02)"
        ));
    }
    if path.contains("{workspace_id}") && !under(WORKSPACE) {
        messages.push(format!(
            "{here} names {{workspace_id}} outside {WORKSPACE} (API-02)"
        ));
    }
    if under(CALLER) && access == "public" {
        messages.push(format!(
            "{here} acts on the caller's own resources, so it isn't public (API-02)"
        ));
    }
    for parameter in parameters(document, operation, shared) {
        let name = parameter["name"].as_str().unwrap_or_default();
        if parameter["in"] == "header" && name.to_ascii_lowercase().contains("workspace") {
            messages.push(format!(
                "{here} takes the header {name}: the path names the workspace, never a header (API-02)"
            ));
        }
    }
    messages
}

/// API-03: the methods it lists, each with its meaning: `GET` and `DELETE` take no body, `PATCH`
/// takes a JSON Merge Patch, `PUT` the whole resource, and `POST` acts on a collection or a named
/// sub-resource, never on an item's ID.
fn meaning(path: &str, method: &str, operation: &Value, here: &str) -> Vec<String> {
    if !METHODS.contains(&method) {
        return vec![format!(
            "{here}: API-03 gives {} no meaning in this API (API-03)",
            method.to_ascii_uppercase()
        )];
    }
    let body = operation.get("requestBody");
    let mut messages = Vec::new();
    match method {
        "get" | "delete" if body.is_some() => {
            messages.push(format!(
                "{here} takes a body, which its method doesn't (API-03)"
            ));
        }
        "patch" => {
            let media: Vec<&String> = body
                .and_then(|body| body["content"].as_object())
                .map(|content| content.keys().collect())
                .unwrap_or_default();
            if media.is_empty() || media.iter().any(|media| *media != MERGE_PATCH) {
                messages.push(format!(
                    "{here} takes {MERGE_PATCH} only, a JSON Merge Patch (RFC 7396) (API-03)"
                ));
            }
        }
        "put" if body.is_none() => {
            messages.push(format!(
                "{here} takes no body, but a PUT writes the whole resource (API-03)"
            ));
        }
        "post" if path.ends_with('}') => {
            messages.push(format!(
                "{here} posts to an item: a POST creates in a collection or acts on a named sub-resource (API-03)"
            ));
        }
        _ => {}
    }
    messages
}

/// API-04: a read answers 200, a create 201 with `Location` and the resource, a delete or an
/// action with nothing to return 204 with no body, and accepted work 202.
fn statuses(method: &str, operation: &Value, here: &str) -> Vec<String> {
    let empty = Map::new();
    let responses = operation["responses"].as_object().unwrap_or(&empty);
    let successes: Vec<(&String, &Value)> = responses
        .iter()
        .filter(|(status, _)| status.starts_with('2'))
        .collect();
    let mut messages = Vec::new();
    if successes.is_empty() {
        messages.push(format!("{here} documents no success status (API-04)"));
    }
    let allowed: &[&str] = match method {
        "get" => &["200"],
        "delete" => &["202", "204"],
        "patch" | "put" => &["200", "204"],
        _ => &SUCCESSES,
    };
    for (status, response) in successes {
        if !SUCCESSES.contains(&status.as_str()) {
            messages.push(format!(
                "{here} answers {status}, which API-04 doesn't list (API-04)"
            ));
            continue;
        }
        if !allowed.contains(&status.as_str()) {
            messages.push(format!(
                "{here} answers {status}, which its method's outcome doesn't (API-04)"
            ));
        }
        let content = response["content"]
            .as_object()
            .is_some_and(|c| !c.is_empty());
        match status.as_str() {
            "201" => {
                let location = response["headers"].as_object().is_some_and(|headers| {
                    headers.keys().any(|h| h.eq_ignore_ascii_case("location"))
                });
                if !location {
                    messages.push(format!(
                        "{here} answers 201 without a documented Location header (API-04)"
                    ));
                }
                if !content {
                    messages.push(format!(
                        "{here} answers 201 without the created resource (API-04)"
                    ));
                }
            }
            "204" if content => {
                messages.push(format!("{here} answers 204 with a body (API-04)"));
            }
            "200" if !content => {
                messages.push(format!(
                    "{here} answers 200 with no body: an outcome with nothing to return answers 204 (API-04)"
                ));
            }
            _ => {}
        }
    }
    messages
}

/// The parameters of an operation and of its path item, with their references resolved.
pub fn parameters<'a>(
    document: &'a Value,
    operation: &'a Value,
    shared: &'a Value,
) -> Vec<&'a Value> {
    [shared, &operation["parameters"]]
        .into_iter()
        .filter_map(Value::as_array)
        .flatten()
        .map(|parameter| super::schemas::resolve(document, parameter))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn messages(path: &str, method: &str, operation: &Value) -> Vec<String> {
        problems(&json!({}), path, method, operation, &Value::Null)
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    fn ok(content: bool) -> Value {
        if content {
            json!({ "description": "x", "content": { "application/json": { "schema": {} } } })
        } else {
            json!({ "description": "x" })
        }
    }

    #[test]
    fn api_02_a_workspace_route_is_under_its_workspace_and_no_header_selects_one() {
        let elsewhere = json!({ "x-access": "workspace", "responses": { "200": ok(true) } });
        assert_eq!(
            messages("/v1/members/{workspace_id}", "get", &elsewhere),
            [
                "GET /v1/members/{workspace_id} acts on a workspace, so its path starts with /v1/workspaces/{workspace_id} (API-02)",
                "GET /v1/members/{workspace_id} names {workspace_id} outside /v1/workspaces/{workspace_id} (API-02)",
            ]
        );
        let header = json!({
            "x-access": "user",
            "parameters": [{ "name": "X-Workspace-Id", "in": "header" }],
            "responses": { "200": ok(true) },
        });
        assert_eq!(
            messages("/v1/files", "get", &header),
            [
                "GET /v1/files takes the header X-Workspace-Id: the path names the workspace, never a header (API-02)"
            ]
        );
        let members = json!({ "x-access": "workspace", "responses": { "200": ok(true) } });
        assert_eq!(
            messages("/v1/workspaces/{workspace_id}/members", "get", &members),
            Vec::<String>::new()
        );
    }

    #[test]
    fn api_02_the_caller_s_own_resources_need_a_caller() {
        let public = json!({ "x-access": "public", "responses": { "200": ok(true) } });
        assert_eq!(
            messages("/v1/users/me/sessions", "get", &public),
            [
                "GET /v1/users/me/sessions acts on the caller's own resources, so it isn't public (API-02)"
            ]
        );
        assert_eq!(
            messages(
                "/v1/users",
                "post",
                &json!({ "x-access": "public", "requestBody": {}, "responses": { "201": {
            "description": "x", "headers": { "location": {} }, "content": { "application/json": {} },
        } } })
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn api_03_methods_carry_their_meaning() {
        let with_body = json!({ "requestBody": { "content": { "application/json": {} } }, "responses": { "200": ok(true) } });
        assert_eq!(
            messages("/v1/things", "get", &with_body),
            ["GET /v1/things takes a body, which its method doesn't (API-03)"]
        );
        assert_eq!(
            messages("/v1/things/{thing_id}", "patch", &with_body),
            [
                "PATCH /v1/things/{thing_id} takes application/merge-patch+json only, a JSON Merge Patch (RFC 7396) (API-03)"
            ]
        );
        assert_eq!(
            messages("/v1/things/{thing_id}", "post", &with_body),
            [
                "POST /v1/things/{thing_id} posts to an item: a POST creates in a collection or acts on a named sub-resource (API-03)"
            ]
        );
        let bodiless = json!({ "responses": { "204": ok(false) } });
        assert_eq!(
            messages("/v1/things/{thing_id}", "put", &bodiless),
            [
                "PUT /v1/things/{thing_id} takes no body, but a PUT writes the whole resource (API-03)"
            ]
        );
        assert_eq!(
            messages("/v1/things", "options", &bodiless),
            ["OPTIONS /v1/things: API-03 gives OPTIONS no meaning in this API (API-03)"]
        );
        let merge = json!({ "requestBody": { "content": { MERGE_PATCH: {} } }, "responses": { "200": ok(true) } });
        assert_eq!(
            messages("/v1/users/me", "patch", &merge),
            Vec::<String>::new()
        );
    }

    #[test]
    fn api_04_statuses_follow_the_outcome() {
        let create = json!({ "requestBody": {}, "responses": { "201": ok(false) } });
        assert_eq!(
            messages("/v1/things", "post", &create),
            [
                "POST /v1/things answers 201 without a documented Location header (API-04)",
                "POST /v1/things answers 201 without the created resource (API-04)",
            ]
        );
        let delete = json!({ "responses": { "200": ok(true) } });
        assert_eq!(
            messages("/v1/things/{thing_id}", "delete", &delete),
            [
                "DELETE /v1/things/{thing_id} answers 200, which its method's outcome doesn't (API-04)"
            ]
        );
        let empty_204 = json!({ "responses": { "204": ok(true) } });
        assert_eq!(
            messages("/v1/things/{thing_id}", "delete", &empty_204),
            ["DELETE /v1/things/{thing_id} answers 204 with a body (API-04)"]
        );
        let read = json!({ "responses": { "201": { "description": "x", "headers": { "Location": {} }, "content": { "application/json": {} } }, "200": ok(false) } });
        assert_eq!(
            messages("/v1/things", "get", &read),
            [
                "GET /v1/things answers 200 with no body: an outcome with nothing to return answers 204 (API-04)",
                "GET /v1/things answers 201, which its method's outcome doesn't (API-04)",
            ]
        );
        let none = json!({ "responses": { "404": ok(true), "206": ok(true) } });
        assert_eq!(
            messages("/v1/things", "post", &none),
            ["POST /v1/things answers 206, which API-04 doesn't list (API-04)"]
        );
        let nothing = json!({ "responses": { "404": ok(true) } });
        assert_eq!(
            messages("/v1/things", "post", &nothing),
            ["POST /v1/things documents no success status (API-04)"]
        );
    }
}
