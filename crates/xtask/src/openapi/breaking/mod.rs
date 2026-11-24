//! The breaking-change diff of `check-openapi`: what the committed document takes away or
//! changes of the document that the last final release holds (API-18), and a code that it
//! changes (ERR-02). The last final release is the tag `v<X.Y.Z>` of the newest version, by
//! semantic versioning; a repository without one, such as a fork before its first, has nothing
//! to keep. The diff reads the tag from git, so a checkout needs its tags: CI's fetches them.
//!
//! Of each path of the release's document, a finding is its path item's `servers`, where the
//! committed document's path item holds others, or holds them where the release's holds none, or
//! none where it holds some; the diff compares nothing of a path item's `summary`, `description`
//! and extensions.
//!
//! Of each operation of the release's document, a method under a path, a finding is:
//! - the operation, where the committed document has none under that path and method;
//! - its access level, `x-access`, where the committed document's is another, and is neither
//!   `public`, which takes every caller, nor, where the release's isn't `public`, `user`, which
//!   takes every caller signed in;
//! - its permission, `x-permission`, where the release's operation names one and the committed
//!   document's, at the same access level, names another or none;
//! - a parameter, by where it stands and its name, among the operation's and its path item's,
//!   of which the operation's own takes the place of the path item's of that place and name: one
//!   that is gone, or whose `style` or `explode` differs; one that is required now and wasn't, a
//!   new one included; and what the parameter's schema breaks as a request's (`schema`);
//! - a body that is gone, or required now where it wasn't or where there was none; a content
//!   type of the body, or a content type's schema, that is gone; and what the schema breaks
//!   as a request's;
//! - of each response whose status is below 400: the response, a header or a content type of
//!   it that is gone, a content type's schema, or its `x-itemSchema`, the schema of a stream's
//!   items, that is gone, and what either breaks as a response's;
//! - of the operation's codes, which are the values that the `enum` of the property `code`
//!   lists in the `application/problem+json` schema of a response whose status is 400 or above:
//!   a status with which the release's operation answers a code and this one, which answers
//!   the code, doesn't; and the codes that the release's operation answers and this one
//!   doesn't, where this one answers a code that the release's doesn't. The diff can't tell a
//!   code that changed from an error that is gone beside one that is new, and takes a code that
//!   is gone alone for a request that is taken where it was refused.
//!
//! It refuses what it can't read: a key of a path item that `forms` refuses too, a parameter
//! without a place and a name, a response whose status is no number or that refers to none of
//! the document's, and what `shape` can't read of a schema.

mod schema;
mod shape;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde_json::Value;

use self::{
    schema::{Comparison, Place, Side},
    shape::Shape,
};
use super::{DOCUMENT, METHODS, forms};
use crate::{Finding, release, repo::Repo};

/// The content type of a problem (API-07).
const PROBLEM: &str = "application/problem+json";

/// What the committed document of `repo` breaks of the one that the last final release holds:
/// a report of what it compared, and the findings.
pub fn check(repo: &Repo) -> (Vec<String>, Vec<Finding>) {
    let current = repo
        .file(DOCUMENT)
        .and_then(|file| serde_json::from_str::<Value>(&file.text).ok());
    let Some(current) = current else {
        // `check` reports a document that is missing or no JSON.
        return (Vec::new(), Vec::new());
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let failed = |error: String| (Vec::new(), vec![Finding::new(DOCUMENT, 1, error)]);
    let (version, text) = match release::openapi_in_last_final_release(&root) {
        Ok(Some(release)) => release,
        Ok(None) => {
            let report = "check-openapi: no final release holds a document yet.".to_owned();
            return (vec![report], Vec::new());
        }
        Err(error) => return failed(error),
    };
    let release: Value = match serde_json::from_str(&text) {
        Ok(release) => release,
        Err(error) => {
            return failed(format!(
                "v{version} holds a document that is no JSON: {error}"
            ));
        }
    };
    let findings = broken(&release, &current)
        .into_iter()
        .map(|message| Finding::new(DOCUMENT, 1, format!("{message}, against v{version}")))
        .collect();
    let report = format!("check-openapi: compared with the document of v{version}.");
    (vec![report], findings)
}

/// What `current` breaks of `release`, each a document.
fn broken<'a>(release: &'a Value, current: &'a Value) -> Vec<String> {
    let mut findings = Vec::new();
    let paths = |document: &'a Value| document["paths"].as_object().into_iter().flatten();
    for (whose, document) in [("the release's", release), ("the", current)] {
        for (path, item) in paths(document) {
            for key in item.as_object().into_iter().flatten().map(|(key, _)| key) {
                if !forms::reads_in_path_item(key) {
                    findings.push(format!(
                        "`{path}` of {whose} document holds `{key}`, which the diff can't read \
                         (API-18)"
                    ));
                }
            }
        }
    }
    for (path, item) in paths(release) {
        let of_path = &current["paths"][path];
        if item.get("servers") != of_path.get("servers") {
            findings.push(format!("`{path}` has other `servers` (API-18)"));
        }
        for method in METHODS {
            let Some(of_release) = item.get(method) else {
                continue;
            };
            let name = format!("`{} {path}`", method.to_uppercase());
            let Some(of_current) = of_path.get(method) else {
                findings.push(format!("{name} is gone (API-18)"));
                continue;
            };
            let mut operation = Operation {
                comparison: Comparison::new(release, current),
                release: of_release,
                current: of_current,
                items: [item, of_path],
                codes: Vec::new(),
            };
            operation.access();
            operation.permission();
            operation.parameters();
            operation.body();
            operation.responses();
            operation.codes();
            findings.extend(
                operation
                    .comparison
                    .findings
                    .into_iter()
                    .map(|finding| format!("{name}: {finding} (API-18)")),
            );
            findings.extend(
                operation
                    .codes
                    .into_iter()
                    .map(|finding| format!("{name} {finding} (ERR-02)")),
            );
        }
    }
    findings
}

/// One operation, as the release's document and the committed one hold it.
struct Operation<'a> {
    comparison: Comparison<'a>,
    release: &'a Value,
    current: &'a Value,
    /// The path items that hold the operation, the release's and the committed document's.
    items: [&'a Value; 2],
    /// What the operation breaks of its codes.
    codes: Vec<String>,
}

impl<'a> Operation<'a> {
    fn finding(&mut self, finding: String) {
        self.comparison.findings.push(finding);
    }

    /// The access level, where the document's may refuse a caller that the release's took.
    fn access(&mut self) {
        let (was, now) = (&self.release["x-access"], &self.current["x-access"]);
        if now != was && now != "public" && (now != "user" || was == "public") {
            let level = |access: &Value| access.as_str().unwrap_or_default().to_owned();
            self.finding(format!(
                "the access level `{}` is `{}` now",
                level(was),
                level(now)
            ));
        }
    }

    /// The permission, where the document's operation, at the release's access level, names
    /// another than the release's or none: a caller whose role holds the release's may hold no
    /// other.
    fn permission(&mut self) {
        let (was, now) = (&self.release["x-permission"], &self.current["x-permission"]);
        if was.is_string() && now != was && self.release["x-access"] == self.current["x-access"] {
            let name = |permission: &Value| permission.as_str().unwrap_or("none").to_owned();
            self.finding(format!(
                "the permission `{}` is `{}` now",
                name(was),
                name(now)
            ));
        }
    }

    fn parameters(&mut self) {
        let [release_item, current_item] = self.items;
        let (of_release, of_current) = (
            self.named(release_item, self.release, "the release's"),
            self.named(current_item, self.current, "the document's"),
        );
        for ((place, name), parameter) in &of_release {
            let here = format!("the {place} parameter `{name}`");
            let Some(now) = of_current.get(&(*place, *name)) else {
                self.finding(format!("{here} is gone"));
                continue;
            };
            for key in ["style", "explode"] {
                if parameter.get(key) != now.get(key) {
                    self.finding(format!("{here} has another `{key}`"));
                }
            }
            self.comparison.compare(
                &Place::root(here),
                Side::Request,
                &parameter.get("schema").into_iter().collect::<Vec<_>>(),
                &now.get("schema").into_iter().collect::<Vec<_>>(),
            );
        }
        for ((place, name), parameter) in &of_current {
            let was = of_release.get(&(*place, *name));
            let required = |parameter: &Value| parameter["required"] == true;
            if required(parameter) && !was.is_some_and(|was| required(was)) {
                let new = if was.is_some() { "" } else { "new and " };
                self.finding(format!(
                    "the {place} parameter `{name}` is {new}required now"
                ));
            }
        }
    }

    /// The parameters of `operation` and of `item`, the path item that holds it, each by its
    /// place and its name, of which the operation's take the place of the path item's.
    fn named(
        &mut self,
        item: &'a Value,
        operation: &'a Value,
        document: &str,
    ) -> BTreeMap<(&'a str, &'a str), &'a Value> {
        let mut named = BTreeMap::new();
        let parameters = [item, operation]
            .into_iter()
            .flat_map(|holder| holder["parameters"].as_array().into_iter().flatten());
        for parameter in parameters {
            match (parameter["in"].as_str(), parameter["name"].as_str()) {
                (Some(place), Some(name)) => {
                    named.insert((place, name), parameter);
                }
                _ => self.finding(format!(
                    "a parameter of {document} has no place and name, which the diff can't read"
                )),
            }
        }
        named
    }

    fn body(&mut self) {
        let required = |body: &Value| body["required"] == true;
        let (was, now) = (
            self.release.get("requestBody"),
            self.current.get("requestBody"),
        );
        match (was, now) {
            (Some(_), None) => self.finding("the body is gone".to_owned()),
            (was, Some(now)) if required(now) && !was.is_some_and(required) => {
                self.finding("a body is required now".to_owned());
            }
            _ => {}
        }
        let (Some(was), Some(now)) = (was, now) else {
            return;
        };
        self.contents(Side::Request, "the body", was, now);
    }

    /// Compares the schema of each content type of `was`, a body or a response, with that of
    /// the same type of `now`.
    fn contents(&mut self, side: Side, here: &str, was: &'a Value, now: &'a Value) {
        for (media, content) in was["content"].as_object().into_iter().flatten() {
            let Some(of_now) = now["content"].get(media) else {
                self.finding(format!("{here} of `{media}` is gone"));
                continue;
            };
            for (key, what) in [
                ("schema", here.to_owned()),
                ("x-itemSchema", format!("an item of {here}")),
            ] {
                let (was, now) = (content.get(key), of_now.get(key));
                if was.is_some() && now.is_none() {
                    self.finding(format!("{what} of `{media}` has no `{key}` now"));
                    continue;
                }
                self.comparison.compare(
                    &Place::root(format!("{what} of `{media}`")),
                    side,
                    &was.into_iter().collect::<Vec<_>>(),
                    &now.into_iter().collect::<Vec<_>>(),
                );
            }
        }
    }

    fn responses(&mut self) {
        let was = responses(self.comparison.release, self.release);
        let now = responses(self.comparison.current, self.current);
        for (status, response) in &was {
            let (Some(response), Ok(number)) = (response, status.parse::<u16>()) else {
                self.finding(format!(
                    "the release's response `{status}` is one that the diff can't read"
                ));
                continue;
            };
            if number >= 400 {
                continue;
            }
            let here = format!("the {status} response");
            let Some(Some(of_now)) = now.get(status) else {
                self.finding(format!("{here} is gone"));
                continue;
            };
            for header in response["headers"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(name, _)| name)
            {
                if of_now["headers"].get(header).is_none() {
                    self.finding(format!("{here}'s header `{header}` is gone"));
                }
            }
            self.contents(Side::Response, &here, response, of_now);
        }
        for (status, response) in &now {
            if response.is_none() || status.parse::<u16>().is_err() {
                self.finding(format!(
                    "the document's response `{status}` is one that the diff can't read"
                ));
            }
        }
    }

    fn codes(&mut self) {
        let was = self.answered(self.comparison.release, self.release);
        let now = self.answered(self.comparison.current, self.current);
        let mut gone = Vec::new();
        for (code, statuses) in &was {
            match now.get(code) {
                None => gone.push(code.as_str()),
                Some(of_now) => {
                    for status in statuses.difference(of_now) {
                        self.codes
                            .push(format!("no longer answers {code} with a {status}"));
                    }
                }
            }
        }
        let new: Vec<&str> = now
            .keys()
            .filter(|code| !was.contains_key(*code))
            .map(String::as_str)
            .collect();
        if !gone.is_empty() && !new.is_empty() {
            self.codes.push(format!(
                "no longer answers {} and answers {}, which it didn't: a code never changes, \
                 and an error that is gone is released before a new one of its operation",
                gone.join(", "),
                new.join(", ")
            ));
        }
    }

    /// The codes that `operation`, of `document`, answers, each with its statuses, as JSON
    /// writes the code.
    fn answered(
        &mut self,
        document: &'a Value,
        operation: &'a Value,
    ) -> BTreeMap<String, BTreeSet<&'a str>> {
        let mut answered: BTreeMap<String, BTreeSet<&'a str>> = BTreeMap::new();
        for (status, response) in responses(document, operation) {
            let problem = response
                .filter(|_| status.parse::<u16>().is_ok_and(|number| number >= 400))
                .and_then(|response| response["content"][PROBLEM].get("schema"));
            let Some(problem) = problem else {
                continue;
            };
            let codes = Shape::of(document, &[problem]).and_then(|problem| {
                let code = problem
                    .properties
                    .get("code")
                    .map_or(&[][..], Vec::as_slice);
                Ok(Shape::of(document, code)?.values.unwrap_or_default())
            });
            match codes {
                Ok(codes) => {
                    for code in codes {
                        answered.entry(code).or_default().insert(status);
                    }
                }
                Err(unread) => self.finding(format!(
                    "the {status} response: {unread}, which the diff can't read"
                )),
            }
        }
        answered
    }
}

/// The responses of `operation`, of `document`, by status: each as it stands, or the response
/// of the document that it refers to, or `None` where it refers to none.
fn responses<'a>(
    document: &'a Value,
    operation: &'a Value,
) -> BTreeMap<&'a str, Option<&'a Value>> {
    operation["responses"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(status, response)| {
            let response = match response.get("$ref") {
                None => Some(response),
                Some(reference) => reference
                    .as_str()
                    .and_then(|target| target.strip_prefix("#/components/responses/"))
                    .and_then(|name| document["components"]["responses"].get(name)),
            };
            (status.as_str(), response)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A document with one operation, `PATCH /v1/things/{thing_id}`, and the schemas and the
    /// response that it refers to.
    fn document() -> Value {
        json!({
            "paths": {"/v1/things/{thing_id}": {"patch": {
                "parameters": [
                    {"in": "path", "name": "thing_id", "required": true,
                     "schema": {"type": "string", "format": "uuid"}},
                    {"in": "query", "name": "limit", "required": false,
                     "schema": {"type": "integer", "maximum": 100, "default": 50}}
                ],
                "requestBody": {"required": true, "content": {
                    "application/merge-patch+json": {"schema": {"$ref": "#/components/schemas/Change"}}
                }},
                "responses": {
                    "200": {"headers": {"etag": {}}, "content": {
                        "application/json": {"schema": {"$ref": "#/components/schemas/Thing"}}
                    }},
                    "404": {"$ref": "#/components/responses/NotFound"},
                    "409": {"content": {"application/problem+json": {"schema": {"allOf": [
                        {"$ref": "#/components/schemas/Problem"},
                        {"properties": {"code": {
                            "enum": ["things.name_taken", "things.locked"], "type": "string"
                        }}}
                    ]}}}}
                }
            }}},
            "components": {
                "schemas": {
                    "Change": {"type": "object", "additionalProperties": false, "required": ["name"],
                               "properties": {"name": {"type": "string", "maxLength": 50}}},
                    "Thing": {"type": "object", "required": ["id", "name"],
                              "properties": {"id": {"type": "string"}, "name": {"type": "string"}}},
                    "Problem": {"type": "object", "properties": {"code": {"type": "string"}}}
                },
                "responses": {"NotFound": {"content": {"application/problem+json": {"schema": {"allOf": [
                    {"$ref": "#/components/schemas/Problem"},
                    {"properties": {"code": {"enum": ["request.not_found"], "type": "string"}}}
                ]}}}}}
            }
        })
    }

    /// The operation of `document`.
    fn operation(document: &mut Value) -> &mut Value {
        &mut document["paths"]["/v1/things/{thing_id}"]["patch"]
    }

    /// What `changed`, the document after `change`, breaks of the document.
    fn broken_by(change: impl FnOnce(&mut Value)) -> Vec<String> {
        let mut changed = document();
        change(&mut changed);
        broken(&document(), &changed)
    }

    const NAME: &str = "`PATCH /v1/things/{thing_id}`";

    #[test]
    fn api_18_a_document_that_only_grows_breaks_nothing() {
        let found = broken_by(|document| {
            document["paths"]["/v1/things"] = json!({"get": {"responses": {"200": {}}}});
            document["paths"]["/v1/things/{thing_id}"]["delete"] =
                json!({"responses": {"204": {}}});
            let operation = operation(document);
            operation["parameters"]
                .as_array_mut()
                .unwrap()
                .push(json!({"in": "header", "name": "if-match", "required": false, "schema": {"type": "string"}}));
            operation["requestBody"]["content"]["application/json"] =
                json!({"schema": {"type": "object"}});
            operation["responses"]["202"] = json!({});
            operation["responses"]["200"]["headers"]["last-modified"] = json!({});
            operation["responses"]["200"]["content"]["text/csv"] =
                json!({"schema": {"type": "string"}});
            document["components"]["schemas"]["Change"]["properties"]["note"] =
                json!({"type": "string"});
            document["components"]["schemas"]["Thing"]["properties"]["note"] =
                json!({"type": "string"});
        });

        assert_eq!(found, Vec::<String>::new());
    }

    #[test]
    fn api_18_an_operation_of_the_release_stays() {
        let moved = broken_by(|document| {
            let operation = operation(document).take();
            document["paths"]["/v1/things/{thing_id}"] = json!({"put": operation});
        });
        let gone = broken_by(|document| {
            document["paths"] = json!({});
        });

        assert_eq!(moved, [format!("{NAME} is gone (API-18)")]);
        assert_eq!(gone, moved);
    }

    #[test]
    fn api_18_an_operation_s_access_level_takes_each_caller_that_it_took() {
        let changed = |was: &str, now: &str| {
            let (mut release, mut current) = (document(), document());
            operation(&mut release)["x-access"] = json!(was);
            operation(&mut current)["x-access"] = json!(now);
            broken(&release, &current)
        };

        assert_eq!(
            changed("public", "user"),
            [format!(
                "{NAME}: the access level `public` is `user` now (API-18)"
            )]
        );
        assert_eq!(
            changed("user", "workspace"),
            [format!(
                "{NAME}: the access level `user` is `workspace` now (API-18)"
            )]
        );
        assert_eq!(changed("user", "public"), Vec::<String>::new());
        assert_eq!(changed("workspace", "user"), Vec::<String>::new());
        assert_eq!(changed("workspace", "workspace"), Vec::<String>::new());
    }

    #[test]
    fn api_18_an_operation_s_permission_stays_at_its_access_level() {
        let changed = |was: (&str, Option<&str>), now: (&str, Option<&str>)| {
            let (mut release, mut current) = (document(), document());
            for (document, (access, permission)) in [(&mut release, was), (&mut current, now)] {
                let operation = operation(document);
                operation["x-access"] = json!(access);
                if let Some(permission) = permission {
                    operation["x-permission"] = json!(permission);
                }
            }
            broken(&release, &current)
        };
        let read = ("workspace", Some("things.read"));

        assert_eq!(
            changed(read, ("workspace", Some("things.update"))),
            [format!(
                "{NAME}: the permission `things.read` is `things.update` now (API-18)"
            )]
        );
        assert_eq!(
            changed(read, ("workspace", None)),
            [format!(
                "{NAME}: the permission `things.read` is `none` now (API-18)"
            )]
        );
        assert_eq!(changed(read, read), Vec::<String>::new());
        assert_eq!(changed(read, ("user", None)), Vec::<String>::new());
    }

    #[test]
    fn api_18_a_path_item_s_parameters_are_each_of_its_operations_and_its_servers_stay() {
        let path = "/v1/things/{thing_id}";
        let moved = broken_by(|document| {
            let limit = operation(document)["parameters"]
                .as_array_mut()
                .unwrap()
                .remove(1);
            let item = &mut document["paths"][path];
            item["parameters"] = json!([limit]);
            item["summary"] = json!("A thing.");
            item["description"] = json!("The thing.");
            item["x-group"] = json!("things");
        });
        // The path item's `limit` is required, but the operation's own takes its place.
        let shadowed = broken_by(|document| {
            let mut required = operation(document)["parameters"][1].clone();
            required["required"] = json!(true);
            document["paths"][path]["parameters"] = json!([required]);
        });
        let (mut release, current) = (document(), document());
        release["paths"][path]["parameters"] =
            json!([{"in": "header", "name": "x-tag", "schema": {"type": "string"}}]);
        let gone = broken(&release, &current);
        let served = broken_by(|document| {
            document["paths"][path]["servers"] = json!([{"url": "/elsewhere"}]);
        });

        assert_eq!(moved, Vec::<String>::new());
        assert_eq!(shadowed, Vec::<String>::new());
        assert_eq!(
            gone,
            [format!(
                "{NAME}: the header parameter `x-tag` is gone (API-18)"
            )]
        );
        assert_eq!(served, [format!("`{path}` has other `servers` (API-18)")]);
    }

    #[test]
    fn api_18_a_parameter_stays_as_it_is_taken_and_none_is_required_now() {
        let found = broken_by(|document| {
            *operation(document).pointer_mut("/parameters").unwrap() = json!([
                {"in": "path", "name": "thing_id", "required": true, "schema": {"type": "string"}},
                {"in": "query", "name": "limit", "required": true, "style": "form",
                 "schema": {"type": "integer", "maximum": 50, "default": 50}},
                {"in": "header", "name": "limit", "required": true, "schema": {"type": "string"}},
                {"in": "query", "name": "search", "required": false, "schema": {"type": "string"}}
            ]);
        });
        let gone = broken_by(|document| {
            operation(document)["parameters"] = json!([]);
        });

        assert_eq!(
            found,
            [
                format!("{NAME}: the query parameter `limit` has another `style` (API-18)"),
                format!(
                    "{NAME}: the query parameter `limit`: `maximum` is 50 where it was 100 (API-18)"
                ),
                format!("{NAME}: the header parameter `limit` is new and required now (API-18)"),
                format!("{NAME}: the query parameter `limit` is required now (API-18)"),
            ]
        );
        assert_eq!(
            gone,
            [
                format!("{NAME}: the path parameter `thing_id` is gone (API-18)"),
                format!("{NAME}: the query parameter `limit` is gone (API-18)"),
            ]
        );
    }

    #[test]
    fn api_18_a_body_stays_as_it_is_taken_and_none_is_required_now() {
        let gone = broken_by(|document| {
            operation(document)
                .as_object_mut()
                .unwrap()
                .remove("requestBody");
        });
        let another = broken_by(|document| {
            let schema =
                operation(document)["requestBody"]["content"]["application/merge-patch+json"]
                    .take();
            operation(document)["requestBody"]["content"] = json!({"application/json": schema});
        });
        let narrower = broken_by(|document| {
            document["components"]["schemas"]["Change"]["properties"]["name"]["maxLength"] =
                json!(40);
            document["components"]["schemas"]["Change"]["properties"]["kind"] =
                json!({"type": "string"});
            document["components"]["schemas"]["Change"]["required"] = json!(["name", "kind"]);
        });
        let optional = |required: Value| {
            let mut release = document();
            operation(&mut release)["requestBody"]["required"] = required;
            broken(&release, &document())
        };
        let mut without = document();
        operation(&mut without)
            .as_object_mut()
            .unwrap()
            .remove("requestBody");

        assert_eq!(gone, [format!("{NAME}: the body is gone (API-18)")]);
        assert_eq!(
            another,
            [format!(
                "{NAME}: the body of `application/merge-patch+json` is gone (API-18)"
            )]
        );
        assert_eq!(
            narrower,
            [
                format!(
                    "{NAME}: the body of `application/merge-patch+json`'s `name`: `maxLength` is 40 where it was 50 (API-18)"
                ),
                format!(
                    "{NAME}: the body of `application/merge-patch+json`'s `kind` is new and required now (API-18)"
                ),
            ]
        );
        assert_eq!(
            optional(json!(false)),
            [format!("{NAME}: a body is required now (API-18)")]
        );
        assert_eq!(
            optional(Value::Null),
            [format!("{NAME}: a body is required now (API-18)")]
        );
        assert_eq!(
            broken(&without, &document()),
            [format!("{NAME}: a body is required now (API-18)")]
        );
        let unsaid = broken_by(|document| {
            operation(document)["requestBody"]["content"]["application/merge-patch+json"] =
                json!({});
        });
        assert_eq!(
            unsaid,
            [format!(
                "{NAME}: the body of `application/merge-patch+json` has no `schema` now (API-18)"
            )]
        );
        // A body that a request may leave out is growth, and so is one that is optional now.
        let mut takes = document();
        operation(&mut takes)["requestBody"]["required"] = json!(false);
        assert_eq!(broken(&without, &takes), Vec::<String>::new());
        assert_eq!(broken(&document(), &takes), Vec::<String>::new());
    }

    #[test]
    fn api_18_a_response_below_400_stays_with_what_it_holds() {
        let gone = broken_by(|document| {
            let response = operation(document)["responses"]["200"].take();
            operation(document)["responses"]
                .as_object_mut()
                .unwrap()
                .remove("200");
            operation(document)["responses"]["201"] = response;
        });
        let less = broken_by(|document| {
            operation(document)["responses"]["200"]["headers"] = json!({});
            document["components"]["schemas"]["Thing"]["required"] = json!(["id"]);
            document["components"]["schemas"]["Thing"]["properties"]["id"]["type"] =
                json!(["string", "null"]);
        });
        let another = broken_by(|document| {
            operation(document)["responses"]["200"]["content"] = json!({"text/plain": {}});
        });
        let unsaid = broken_by(|document| {
            operation(document)["responses"]["200"]["content"]["application/json"] = json!({});
        });
        assert_eq!(
            unsaid,
            [format!(
                "{NAME}: the 200 response of `application/json` has no `schema` now (API-18)"
            )]
        );
        // A response of 400 or above that is gone is a request that is taken.
        let taken = broken_by(|document| {
            let responses = operation(document)["responses"].as_object_mut().unwrap();
            responses.remove("404");
            responses.remove("409");
        });

        assert_eq!(gone, [format!("{NAME}: the 200 response is gone (API-18)")]);
        assert_eq!(
            less,
            [
                format!("{NAME}: the 200 response's header `etag` is gone (API-18)"),
                format!(
                    "{NAME}: the 200 response of `application/json`'s `id` may be null now (API-18)"
                ),
                format!(
                    "{NAME}: the 200 response of `application/json`'s `name` may be absent now (API-18)"
                ),
            ]
        );
        assert_eq!(
            another,
            [format!(
                "{NAME}: the 200 response of `application/json` is gone (API-18)"
            )]
        );
        assert_eq!(taken, Vec::<String>::new());
    }

    #[test]
    fn api_18_a_stream_s_items_stay_what_they_were() {
        let stream = |frame: Value| {
            let mut document = document();
            operation(&mut document)["responses"]["200"]["content"] = json!({"text/event-stream": {
                "schema": {"type": "string"},
                "x-itemSchema": {"$ref": "#/components/schemas/Frame"}
            }});
            document["components"]["schemas"]["Frame"] = frame;
            document
        };
        let frame =
            json!({"properties": {"event": {"enum": ["thing.renamed"]}}, "required": ["event"]});
        let renamed =
            json!({"properties": {"event": {"enum": ["thing.changed"]}}, "required": ["event"]});
        let mut without = stream(frame.clone());
        operation(&mut without)["responses"]["200"]["content"]["text/event-stream"]
            .as_object_mut()
            .unwrap()
            .remove("x-itemSchema");

        assert_eq!(
            broken(&stream(frame.clone()), &stream(frame.clone())),
            Vec::<String>::new()
        );
        assert_eq!(
            broken(&stream(frame.clone()), &stream(renamed)),
            [format!(
                "{NAME}: an item of the 200 response of `text/event-stream`'s `event` is one of \
                 \"thing.changed\" where it was one of \"thing.renamed\" (API-18)"
            )]
        );
        assert_eq!(
            broken(&stream(frame), &without),
            [format!(
                "{NAME}: an item of the 200 response of `text/event-stream` has no `x-itemSchema` \
                 now (API-18)"
            )]
        );
    }

    #[test]
    fn err_02_a_code_keeps_its_name_and_its_status() {
        let codes = |listed: Value| {
            broken_by(|document| {
                operation(document)["responses"]["409"]["content"][PROBLEM]["schema"]["allOf"][1]
                    ["properties"]["code"]["enum"] = listed;
            })
        };
        let moved = broken_by(|document| {
            let response = operation(document)["responses"]["409"].take();
            operation(document)["responses"]
                .as_object_mut()
                .unwrap()
                .remove("409");
            operation(document)["responses"]["422"] = response;
        });
        let referred = broken_by(|document| {
            document["components"]["responses"]["NotFound"]["content"][PROBLEM]["schema"]["allOf"]
                [1]["properties"]["code"]["enum"] = json!(["things.not_found"]);
        });

        // A new code alone is growth, and a code that is gone alone is a request that is taken.
        assert_eq!(
            codes(json!(["things.name_taken", "things.locked", "things.full"])),
            Vec::<String>::new()
        );
        assert_eq!(codes(json!(["things.locked"])), Vec::<String>::new());
        assert_eq!(
            codes(json!(["things.name_in_use", "things.locked"])),
            [format!(
                "{NAME} no longer answers \"things.name_taken\" and answers \
                 \"things.name_in_use\", which it didn't: a code never changes, and an error \
                 that is gone is released before a new one of its operation (ERR-02)"
            )]
        );
        assert_eq!(
            moved,
            [
                format!("{NAME} no longer answers \"things.locked\" with a 409 (ERR-02)"),
                format!("{NAME} no longer answers \"things.name_taken\" with a 409 (ERR-02)"),
            ]
        );
        assert_eq!(
            referred,
            [format!(
                "{NAME} no longer answers \"request.not_found\" and answers \
                 \"things.not_found\", which it didn't: a code never changes, and an error \
                 that is gone is released before a new one of its operation (ERR-02)"
            )]
        );
    }

    #[test]
    fn api_18_what_the_diff_can_t_read_of_an_operation_is_a_finding() {
        let keyed = broken_by(|document| {
            document["paths"]["/v1/things/{thing_id}"]["$ref"] =
                json!("#/components/pathItems/Thing");
        });
        let unnamed = broken_by(|document| {
            operation(document)["parameters"]
                .as_array_mut()
                .unwrap()
                .push(json!({"$ref": "#/components/parameters/Limit"}));
        });
        let unreferred = broken_by(|document| {
            operation(document)["responses"]["404"] =
                json!({"$ref": "#/components/responses/Gone"});
            operation(document)["responses"]["default"] = json!({});
        });
        let untyped = broken_by(|document| {
            document["components"]["schemas"]["Problem"] = json!("a problem");
        });

        assert_eq!(
            keyed,
            [
                "`/v1/things/{thing_id}` of the document holds `$ref`, which the diff can't read (API-18)"
            ]
        );
        assert_eq!(
            unnamed,
            [format!(
                "{NAME}: a parameter of the document's has no place and name, which the diff \
                 can't read (API-18)"
            )]
        );
        assert_eq!(
            unreferred,
            [
                format!(
                    "{NAME}: the document's response `404` is one that the diff can't read (API-18)"
                ),
                format!(
                    "{NAME}: the document's response `default` is one that the diff can't read (API-18)"
                ),
            ]
        );
        assert_eq!(
            untyped,
            [
                format!(
                    "{NAME}: the 404 response: the schema `\"a problem\"` is no object, which the diff can't read (API-18)"
                ),
                format!(
                    "{NAME}: the 409 response: the schema `\"a problem\"` is no object, which the diff can't read (API-18)"
                ),
            ]
        );
    }
}
