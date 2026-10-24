//! `check-docs`, the routes: each module's spec lists, in its Routes table, the operations that
//! `openapi.json` documents for the module, with their access levels (DOC-15). Once the module
//! is built, it lists no others: until then, its spec is the plan for routes it doesn't serve
//! yet. An operation names its module as its first tag.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::{Finding, markdown, repo::Repo, text};

/// The committed document.
const DOCUMENT: &str = "openapi.json";

/// The module specs.
const SPECS: &str = "docs/spec/";

/// The feature modules' code: a module is built once its directory holds a file.
const MODULES: &str = "crates/app/src/modules/";

/// The keys of a path item that hold operations.
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// An operation of the document: its module and its access level.
struct Documented {
    module: String,
    access: String,
}

/// Runs the check. A document that is missing or isn't JSON is `check-openapi`'s to report.
pub fn check(repo: &Repo) -> Vec<Finding> {
    let Some(document) = repo
        .file(DOCUMENT)
        .and_then(|file| serde_json::from_str::<Value>(&file.text).ok())
    else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    let mut documented = operations(&document, repo, &mut findings);
    for file in repo.files() {
        let Some(module) = file
            .path
            .strip_prefix(SPECS)
            .and_then(|name| name.strip_suffix(".md"))
            .filter(|name| *name != "README" && !name.contains('/'))
        else {
            continue;
        };
        let built = repo
            .files()
            .any(|code| code.path.starts_with(&format!("{MODULES}{module}/")));
        for table in markdown::tables(&file.text) {
            if table.section.as_deref() != Some("Routes") {
                continue;
            }
            for (line, cells) in &table.rows {
                let route = cells
                    .first()
                    .map(|cell| cell.trim_matches('`'))
                    .unwrap_or_default();
                let Some((method, path)) = route.split_once(' ') else {
                    continue;
                };
                // The health probes are answered before the routes, and the document leaves them
                // out (OPS-05).
                if !path.starts_with("/v1") {
                    continue;
                }
                let finding = |message: String| Finding::new(&file.path, *line, message);
                match documented.remove(&(method.to_owned(), path.to_owned())) {
                    None if !built => {}
                    None => findings.push(finding(format!(
                        "`{route}` isn't an operation of {DOCUMENT} (DOC-15)"
                    ))),
                    Some(operation) => {
                        if operation.module != module {
                            findings.push(finding(format!(
                                "`{route}` belongs to `{}` in {DOCUMENT}, not to this spec (DOC-15)",
                                operation.module
                            )));
                        }
                        let access = access_level(cells.get(1).map_or("", String::as_str));
                        if access != operation.access {
                            findings.push(finding(format!(
                                "`{route}` is `{}` in {DOCUMENT}, not `{access}` (DOC-15)",
                                operation.access
                            )));
                        }
                    }
                }
            }
        }
    }
    for ((method, path), operation) in documented {
        findings.push(Finding::new(
            &format!("{SPECS}{}.md", operation.module),
            1,
            format!("the Routes table doesn't list `{method} {path}`, which {DOCUMENT} documents (DOC-15)"),
        ));
    }
    findings
}

/// Every operation of the document, by method and path. An operation that names no module
/// with a spec in its tags is a finding.
fn operations(
    document: &Value,
    repo: &Repo,
    findings: &mut Vec<Finding>,
) -> BTreeMap<(String, String), Documented> {
    let empty = Map::new();
    let text = repo.file(DOCUMENT).map_or("", |file| file.text.as_str());
    let mut operations = BTreeMap::new();
    for (path, item) in document["paths"].as_object().unwrap_or(&empty) {
        for method in METHODS {
            let Some(operation) = item.get(method) else {
                continue;
            };
            let method = method.to_ascii_uppercase();
            match operation["tags"][0].as_str() {
                Some(module) if repo.has(&format!("{SPECS}{module}.md")) => {
                    operations.insert(
                        (method, path.clone()),
                        Documented {
                            module: module.to_owned(),
                            access: operation["x-access"]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned(),
                        },
                    );
                }
                _ => findings.push(Finding::new(
                    DOCUMENT,
                    text::line_of(text, &format!("\"{path}\"")),
                    format!(
                        "`{method} {path}` names no module with a spec as its first tag (DOC-15)"
                    ),
                )),
            }
        }
    }
    operations
}

/// The access level a Routes cell states: `public`, `user` or `workspace`, before any `::`
/// that names a permission.
fn access_level(cell: &str) -> String {
    cell.trim_start_matches('`')
        .chars()
        .take_while(char::is_ascii_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOCUMENT_TEXT: &str = r#"{"paths": {
        "/v1/capabilities": {"get": {"tags": ["system"], "x-access": "public"}},
        "/v1/workspaces": {"post": {"tags": ["workspaces"], "x-access": "user"}}
    }}"#;

    fn findings(system: &str, workspaces: &str) -> Vec<String> {
        let repo = Repo::of(&[
            (DOCUMENT, DOCUMENT_TEXT),
            ("docs/spec/system.md", system),
            ("docs/spec/workspaces.md", workspaces),
            ("crates/app/src/modules/system/mod.rs", ""),
        ]);
        check(&repo)
            .into_iter()
            .map(|finding| finding.message)
            .collect()
    }

    fn routes(rows: &str) -> String {
        format!("## Routes\n\n| Method and path | Access | Behaviors |\n|---|---|---|\n{rows}\n")
    }

    #[test]
    fn doc_15_specs_that_list_their_operations_pass() {
        let system = routes(
            "| `GET /health/live` | public, before the stack | PLATFORM-019 |\n| `GET /v1/capabilities` | `public` | SYSTEM-001 |",
        );
        let workspaces = routes("| `POST /v1/workspaces` | `user` | WORKSPACES-001 |");
        assert_eq!(findings(&system, &workspaces), Vec::<String>::new());
    }

    #[test]
    fn doc_15_an_unbuilt_module_s_spec_may_plan_routes_the_document_lacks() {
        let workspaces = routes(
            "| `POST /v1/workspaces` | `user` | WORKSPACES-001 |\n| `GET /v1/workspaces` | `user` | WORKSPACES-003 |",
        );
        let system = routes("| `GET /v1/capabilities` | `public` | SYSTEM-001 |");
        assert_eq!(findings(&system, &workspaces), Vec::<String>::new());
    }

    #[test]
    fn doc_15_an_operation_names_a_module_with_a_spec_as_its_first_tag() {
        let document =
            r#"{"paths": {"/v1/things": {"get": {"tags": ["things"], "x-access": "public"}}}}"#;
        let repo = Repo::of(&[(DOCUMENT, document)]);
        let found: Vec<String> = check(&repo)
            .into_iter()
            .map(|finding| finding.message)
            .collect();
        assert_eq!(
            found,
            ["`GET /v1/things` names no module with a spec as its first tag (DOC-15)"]
        );
    }

    #[test]
    fn doc_15_a_spec_lists_each_operation_of_its_module() {
        let workspaces = routes("| `POST /v1/workspaces` | `user` | WORKSPACES-001 |");
        assert_eq!(
            findings("## Purpose\n", &workspaces),
            [
                "the Routes table doesn't list `GET /v1/capabilities`, which openapi.json documents (DOC-15)"
            ]
        );
    }

    #[test]
    fn doc_15_a_listed_route_matches_the_document() {
        let system = routes(
            "| `GET /v1/capabilities` | `user` | SYSTEM-001 |\n| `DELETE /v1/capabilities` | `public` | SYSTEM-001 |\n| `POST /v1/workspaces` | `user` | SYSTEM-001 |",
        );
        assert_eq!(
            findings(&system, "## Purpose\n"),
            [
                "`GET /v1/capabilities` is `public` in openapi.json, not `user` (DOC-15)",
                "`DELETE /v1/capabilities` isn't an operation of openapi.json (DOC-15)",
                "`POST /v1/workspaces` belongs to `workspaces` in openapi.json, not to this spec (DOC-15)",
            ]
        );
    }
}
