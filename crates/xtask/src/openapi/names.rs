//! `check-openapi`, the names the document keys its parts by (API-17):
//! - no two operations share an `operationId`, which OpenAPI requires to be unique, and which a
//!   client generated from the document names its methods by;
//! - no two types of the code take one schema name, by deriving `ToSchema` under their own name or
//!   under the one `#[schema(as = …)]` gives: utoipa keeps one schema of each name, so every
//!   operation that names the other type would document the first's.
//!
//! It reads the committed document for the first, and the code of `platform` and `app`, before
//! each file's tests, for the second.

use std::collections::BTreeMap;

use proc_macro2::TokenTree;
use serde_json::{Map, Value};

use super::{METHODS, Problem};
use crate::{
    Finding,
    repo::Repo,
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
        // A file that isn't Rust is check-layers' and the compiler's to report.
        let Ok(source) = Source::read(file) else {
            continue;
        };
        let tests = text::lines_before_tests(&file.text);
        for (name, line, attributes) in rust::types_with_attributes(&source.tokens) {
            if line > tests || !attributes.iter().any(|attribute| derives_schema(attribute)) {
                continue;
            }
            let schema = attributes.iter().find_map(|attribute| renamed(attribute));
            let schema = schema.unwrap_or_else(|| name.clone());
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
            "/v1/workspaces": { "post": { "operationId": "create" }, "get": { "operationId": "list" } }
        }});

        let messages: Vec<String> = operation_ids(&document)
            .into_iter()
            .map(|(_, message)| message)
            .collect();

        assert_eq!(
            messages,
            [
                "POST /v1/workspaces/{workspace_id}/orders has the operationId `create` of POST \
              /v1/workspaces, and OpenAPI requires each to be unique: name its handler after what \
              it does to its resource (API-17)"
            ]
        );
    }

    #[test]
    fn api_17_two_types_that_take_one_schema_name_are_reported() {
        let workspaces = "#[derive(Deserialize, ToSchema)]\npub(crate) struct CreateRequest {}\n";
        let orders = "#[derive(Deserialize, ToSchema)]\n#[serde(deny_unknown_fields)]\n\
                      pub(crate) struct CreateRequest {}\n\
                      #[derive(Serialize, ToSchema)]\n#[schema(as = Problem)]\nstruct Body {}\n\
                      #[derive(IntoParams)]\nstruct ListQuery {}\n";
        let platform = "#[derive(Serialize, ToSchema)]\n#[schema(as = Problem, description = \"x\")]\n\
                        struct ProblemBody {}\n\
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
                "crates/platform/src/errors/mod.rs:3"
            ]
        );
    }
}
