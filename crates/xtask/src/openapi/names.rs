//! `check-openapi`, the names the document keys its parts by (API-17):
//! - no two operations share an `operationId`, which OpenAPI requires to be unique, and which a
//!   client generated from the document names its methods by;
//! - no two types of the code take one schema name, by deriving `ToSchema` under their own name or
//!   under the one `#[schema(as = …)]` gives, or by a hand-written `impl ToSchema`: utoipa keeps
//!   one schema of each name, so every operation that names the other type would document the
//!   first's;
//! - no such type takes the schema name of the event catalog or of the action catalog, `Frame`
//!   or `AuditEntry`, or of a component of either: one that the catalog refers to, or that the
//!   document marks as a frame type's or an action's. The document builds each of those under a
//!   name of its own making, and would keep one schema of the two.
//!
//! It reads the committed document for the first and for the catalogs' components, and the code
//! of `platform` and `app`, before each file's tests, for the types.

use std::collections::BTreeMap;

use proc_macro2::TokenTree;
use serde_json::{Map, Value};

use super::{METHODS, Problem, actions, events};
use crate::{
    Finding,
    repo::{File, Repo},
    rust::{self, Source},
    text,
};

/// The code whose types the document's schemas come from.
const CODE: [&str; 2] = ["crates/platform/src/", "crates/app/src/"];

/// Each operation whose `operationId` an earlier operation has.
pub fn operation_ids(document: &Value) -> Vec<Problem> {
    let empty = Map::new();
    let paths = document["paths"].as_object().unwrap_or(&empty);
    let mut first: BTreeMap<&str, String> = BTreeMap::new();
    let mut problems = Vec::new();
    for (path, item) in paths {
        for method in METHODS {
            let Some(id) = item
                .get(method)
                .and_then(|operation| operation["operationId"].as_str())
            else {
                continue;
            };
            let here = format!("{} {path}", method.to_ascii_uppercase());
            match first.get(id) {
                Some(earlier) => problems.push((
                    format!("\"operationId\": \"{id}\""),
                    format!(
                        "{here} has the operationId `{id}` of {earlier}, and OpenAPI requires each \
                         to be unique: name its handler after what it does to its resource (API-17)"
                    ),
                )),
                None => {
                    first.insert(id, here);
                }
            }
        }
    }
    problems
}

/// Each type of the code that takes a schema name an earlier type takes.
pub fn schema_names(repo: &Repo) -> Vec<Finding> {
    let mut first: BTreeMap<String, (String, usize)> = BTreeMap::new();
    let mut findings = Vec::new();
    for file in repo.files() {
        let in_code = CODE.iter().any(|code| file.path.starts_with(code));
        if !in_code || !file.path.ends_with(".rs") {
            continue;
        }
        for (name, schema, line) in schemas_of(file) {
            match first.get(&schema) {
                Some((path, at)) => findings.push(Finding::new(
                    &file.path,
                    line,
                    format!(
                        "`{name}` takes the schema name `{schema}`, which a type at {path}:{at} \
                         takes too: the document keeps one schema of each name, so the operations \
                         of one would document the other's; name the type after its resource \
                         (API-17)"
                    ),
                )),
                None => {
                    first.insert(schema, (file.path.clone(), line));
                }
            }
        }
    }
    findings
}

/// Each type of the code that takes the schema name of a component of one of `document`'s two
/// catalogs: the catalog's own, one that the catalog refers to, or one that the document marks
/// as a frame type's or an action's.
pub fn catalog_names(repo: &Repo, document: &Value) -> Vec<Finding> {
    let empty = Map::new();
    let schemas = document["components"]["schemas"]
        .as_object()
        .unwrap_or(&empty);
    let mut components: BTreeMap<&str, &str> = BTreeMap::new();
    for (catalog, mark, of) in [
        (events::CATALOG, events::MARK, "event"),
        (actions::CATALOG, actions::MARK, "action"),
    ] {
        components.insert(catalog, of);
        for component in events::referred(document, catalog) {
            components.insert(component, of);
        }
        for (component, schema) in schemas {
            if schema[mark].is_string() {
                components.insert(component, of);
            }
        }
    }
    let mut findings = Vec::new();
    for file in repo.files() {
        let in_code = CODE.iter().any(|code| file.path.starts_with(code));
        if !in_code || !file.path.ends_with(".rs") {
            continue;
        }
        for (name, schema, line) in schemas_of(file) {
            if let Some(of) = components.get(schema.as_str()) {
                findings.push(Finding::new(
                    &file.path,
                    line,
                    format!(
                        "`{name}` takes the schema name `{schema}`, which a component of the \
                         {of} catalog takes too: the document keeps one schema of each name, so \
                         it would document one of the two under it; give the type another name \
                         (API-17)"
                    ),
                ));
            }
        }
    }
    findings
}

/// The schema names that the code of `platform` and `app` in `repo` takes.
pub fn taken(repo: &Repo) -> Vec<String> {
    repo.files()
        .filter(|file| {
            CODE.iter().any(|code| file.path.starts_with(code)) && file.path.ends_with(".rs")
        })
        .flat_map(|file| schemas_of(file).into_iter().map(|(_, schema, _)| schema))
        .collect()
}

/// Each type of `file`'s code, before its tests, that takes a schema name, with that name and its
/// line: a type that derives `ToSchema` takes its own name, or the one `#[schema(as = …)]` gives,
/// and a hand-written `impl ToSchema for` a type takes the type's, as every such impl names it.
pub fn schemas_of(file: &File) -> Vec<(String, String, usize)> {
    // A file that isn't Rust is check-layers' and the compiler's to report.
    let Ok(source) = Source::read(file) else {
        return Vec::new();
    };
    let tests = text::lines_before_tests(&file.text);
    let mut found = Vec::new();
    for (name, line, attributes) in rust::types_with_attributes(&source.tokens) {
        if line > tests || !attributes.iter().any(|attribute| derives_schema(attribute)) {
            continue;
        }
        let schema = attributes.iter().find_map(|attribute| renamed(attribute));
        found.push((name.clone(), schema.unwrap_or(name), line));
    }
    found.extend(
        implemented(&source.tokens)
            .into_iter()
            .filter(|(_, line)| *line <= tests)
            .map(|(name, line)| (name.clone(), name, line)),
    );
    found
}

/// Each type that `tokens` implement `ToSchema` for by hand, at any depth, with its line.
fn implemented(tokens: &[TokenTree]) -> Vec<(String, usize)> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(ident) if ident == "ToSchema" => {
                if let (Some(TokenTree::Ident(word)), Some(TokenTree::Ident(name))) =
                    (tokens.get(at + 1), tokens.get(at + 2))
                    && word == "for"
                {
                    found.push((name.to_string(), name.span().start().line));
                }
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                found.extend(implemented(&inner));
            }
            _ => {}
        }
    }
    found
}

/// Whether `attribute` is a `derive` that holds `ToSchema`.
fn derives_schema(attribute: &[TokenTree]) -> bool {
    matches!(attribute.first(), Some(TokenTree::Ident(ident)) if ident == "derive")
        && rust::holds(attribute, "ToSchema")
}

/// The name that `attribute`, a `#[schema(as = …)]`, gives a type's schema, the last segment of
/// its path.
fn renamed(attribute: &[TokenTree]) -> Option<String> {
    let (Some(TokenTree::Ident(ident)), Some(TokenTree::Group(arguments))) =
        (attribute.first(), attribute.get(1))
    else {
        return None;
    };
    if ident != "schema" {
        return None;
    }
    let tokens: Vec<TokenTree> = arguments.stream().into_iter().collect();
    let start = tokens
        .iter()
        .position(|token| matches!(token, TokenTree::Ident(ident) if ident == "as"))?;
    tokens[start + 1..]
        .iter()
        .skip_while(|token| matches!(token, TokenTree::Punct(punct) if punct.as_char() == '='))
        .take_while(|token| !matches!(token, TokenTree::Punct(punct) if punct.as_char() == ','))
        .filter_map(|token| match token {
            TokenTree::Ident(ident) => Some(ident.to_string()),
            _ => None,
        })
        .last()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn api_17_two_operations_with_one_operation_id_are_reported() {
        let document = json!({ "paths": {
            "/v1/workspaces/{workspace_id}/orders": { "post": { "operationId": "create" } },
            "/v1/workspaces/{workspace_id}/invoices": { "post": { "operationId": "create" }, "get": { "operationId": "list" } }
        }});

        let messages: Vec<String> = operation_ids(&document)
            .into_iter()
            .map(|(_, message)| message)
            .collect();

        assert_eq!(
            messages,
            [
                "POST /v1/workspaces/{workspace_id}/orders has the operationId `create` of POST \
              /v1/workspaces/{workspace_id}/invoices, and OpenAPI requires each to be unique: \
              name its handler after what it does to its resource (API-17)"
            ]
        );
    }

    #[test]
    fn api_17_two_types_that_take_one_schema_name_are_reported() {
        let workspaces = "#[derive(Deserialize, ToSchema)]\npub(crate) struct CreateRequest {}\n\
                          impl ToSchema for WorkspaceName {}\n";
        let orders = "#[derive(Deserialize, ToSchema)]\n#[serde(deny_unknown_fields)]\n\
                      pub(crate) struct CreateRequest {}\n\
                      #[derive(Serialize, ToSchema)]\n#[schema(as = Problem)]\nstruct Body {}\n\
                      #[derive(IntoParams)]\nstruct ListQuery {}\n";
        let platform = "#[derive(Serialize, ToSchema)]\n#[schema(as = Problem, description = \"x\")]\n\
                        struct ProblemBody {}\nimpl ToSchema for WorkspaceName {}\n\
                        #[cfg(test)]\nmod tests {\n    #[derive(ToSchema)]\n    struct CreateRequest {}\n}\n";
        let repo = Repo::of(&[
            ("crates/app/src/modules/workspaces/routes.rs", workspaces),
            ("crates/app/src/modules/orders/routes.rs", orders),
            ("crates/platform/src/errors/mod.rs", platform),
        ]);

        let mut found: Vec<String> = schema_names(&repo)
            .into_iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();
        found.sort();

        assert_eq!(
            found,
            [
                "crates/app/src/modules/orders/routes.rs:3",
                "crates/platform/src/errors/mod.rs:3",
                "crates/platform/src/errors/mod.rs:4"
            ]
        );
    }

    #[test]
    fn api_17_a_type_that_takes_the_name_of_a_catalog_s_component_is_reported() {
        // The catalogs' own names, a frame type's component and an action's that their catalogs
        // refer to, and one of each that the document only marks.
        let pictures = "#[derive(Serialize, ToSchema)]\npub(crate) struct Frame {}\n\
                        #[derive(Serialize, ToSchema)]\nstruct ThingMadeFrame {}\n\
                        #[derive(Serialize, ToSchema)]\nstruct ThingKeptFrame {}\n\
                        #[derive(Serialize, ToSchema)]\nstruct ThingMade {}\n";
        let ledgers = "#[derive(Serialize, ToSchema)]\n#[schema(as = AuditEntry)]\nstruct Line {}\n\
                       impl ToSchema for ThingMadeEntry {}\n\
                       impl ToSchema for ThingKeptEntry {}\n\
                       #[derive(Serialize, ToSchema)]\nstruct LoggedEntry {}\n\
                       #[cfg(test)]\nmod tests {\n    #[derive(ToSchema)]\n    struct Frame {}\n}\n";
        let repo = Repo::of(&[
            ("crates/app/src/modules/pictures/model.rs", pictures),
            ("crates/app/src/modules/ledgers/model.rs", ledgers),
            (
                "crates/app/tests/integration/support/streams.rs",
                "#[derive(ToSchema)]\npub struct Frame {}\n",
            ),
        ]);
        let document = json!({ "components": { "schemas": {
            "ThingMadeFrame": { "x-event": "thing.made" },
            "ThingKeptFrame": { "x-event": "thing.kept" },
            "ThingMade": {},
            "Frame": { "oneOf": [{ "$ref": "#/components/schemas/ThingMadeFrame" }] },
            "ThingMadeEntry": { "x-audit-action": "thing.made" },
            "ThingKeptEntry": { "x-audit-action": "thing.kept" },
            "LoggedEntry": {},
            "AuditEntry": { "oneOf": [{ "$ref": "#/components/schemas/ThingMadeEntry" }] },
        } } });

        let found: Vec<String> = catalog_names(&repo, &document)
            .into_iter()
            .map(|finding| {
                let of = if finding.message.contains("the event catalog") {
                    "event"
                } else {
                    "action"
                };
                format!("{}:{}: {of}", finding.path, finding.line)
            })
            .collect();

        assert_eq!(
            found,
            [
                "crates/app/src/modules/pictures/model.rs:2: event",
                "crates/app/src/modules/pictures/model.rs:4: event",
                "crates/app/src/modules/pictures/model.rs:6: event",
                "crates/app/src/modules/ledgers/model.rs:3: action",
                "crates/app/src/modules/ledgers/model.rs:4: action",
                "crates/app/src/modules/ledgers/model.rs:5: action",
            ]
        );
    }
}
