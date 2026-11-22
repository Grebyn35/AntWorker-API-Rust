//! `check-openapi`, the names the document keys its parts by (API-17):
//! - no two operations share an `operationId`, which OpenAPI requires to be unique, and which a
//!   client generated from the document names its methods by;
//! - no two of the schema names that it reads in the code are the same: utoipa keeps one schema
//!   of each name, so every operation that names the other type would document the first's;
//! - none of those names is that of the event catalog or of the action catalog, `Frame` or
//!   `AuditEntry`, of a component that one of them refers to, or of one that the document marks
//!   as a frame type's or an action's. The document builds each of those under a name of its
//!   own making, and would keep one schema of the two;
//! - each of those names whose `ToSchema` a file of `crates/app/src/` writes, through a derive or
//!   an impl, is a component's, or, for a generic type, starts one's, followed by `_` (CODE-15);
//! - each component is one of those names, `Frame`, `AuditEntry`, one that they refer to or that
//!   the document marks as a frame type's or an action's, or an instance of a generic type, its
//!   name followed by `_`, so that the checks above read every schema's name.
//!
//! It reads the committed document for the operations and the components. The schema names it
//! reads in the code are those of the Rust files under `crates/platform/src/` and
//! `crates/app/src/`, as tokens, on the lines before each file's test module, as
//! [`rust::test_module`](crate::rust::test_module) finds it:
//! - of each `struct` or `enum` that a `#[derive(…)]` holding `ToSchema` stands before, the
//!   identifier after its keyword, or the segments of the path that its `#[schema(as = …)]`
//!   gives, before any `<`, joined by `.`, as utoipa joins them; the type is generic when `<`
//!   follows its identifier;
//! - of each `ToSchema for <type>`, at any depth, in a `macro_rules!` too, the type's identifier
//!   when the impl's body has no `fn name`, as utoipa's own `name()` gives it, and otherwise what
//!   the first `Borrowed(…)` among the tokens of that function's body holds: a string literal,
//!   its string; `stringify!($name)`, or no `fn name` for the type `$name`, in a `macro_rules!`
//!   each of whose rules' matchers takes `$name:ident` as its first fragment but those of kinds
//!   `meta` and `vis`, the first identifier of the arguments of each invocation of that macro,
//!   `<macro>!(…)`, after their attributes and a `pub` with its restriction, as `id_type!` and
//!   `filter_values!` take their types' names; and `P::NAME`, where `P:` and the path of a trait
//!   stand between `impl` and the impl's body, the string literal of `const NAME: … = "…";` in
//!   the body of each `<trait> for`, as `LongCode` names its schemas by its kinds' `SCHEMA`.
//!
//! A name's `ToSchema` is written in the file of its derive or its impl: for an invocation's
//! name, in the file of the `macro_rules!`, and for a constant's, in the file of the impl of
//! `ToSchema`. The check refuses, naming it, a file that doesn't parse, a derive of `ToSchema`
//! for a type that a fragment names, and an impl of `ToSchema` whose name it can't read: its type
//! is no identifier or fragment, its `fn name` holds no `Borrowed(…)` of those forms, no
//! `macro_rules!` around it takes `$name` so, an invocation's arguments start with no
//! identifier, `P` has no trait's bound there, or an impl of the trait doesn't give `NAME` as a
//! string literal.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    METHODS, Problem, actions, events, quoted,
    schema_names::{self, Reading, Schema},
};
use crate::{Finding, repo::Repo};

pub use schema_names::read;

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

/// The schema names that the check reads in the code of `platform` and `app` in `repo`.
pub fn code(repo: &Repo) -> Reading {
    read(repo.files().filter(|file| {
        CODE.iter().any(|code| file.path.starts_with(code)) && file.path.ends_with(".rs")
    }))
}

/// The schema names that the check reads in the code of `platform` and `app` in `repo`.
pub fn taken(repo: &Repo) -> Vec<String> {
    code(repo)
        .schemas
        .into_iter()
        .map(|schema| schema.name)
        .collect()
}

/// Every finding of `reading`, the code's: each `ToSchema` it refused, and each name that breaks
/// a rule of the module's comment against the others or against `document`.
pub fn findings(reading: Reading, document: &Value) -> Vec<Finding> {
    let mut findings = reading.refused;
    findings.extend(duplicates(&reading.schemas));
    findings.extend(catalog_names(&reading.schemas, document));
    findings.extend(unlisted(&reading.schemas, document));
    findings
}

/// Each of `schemas` whose name an earlier one takes.
pub fn duplicates(schemas: &[Schema]) -> Vec<Finding> {
    let mut first: BTreeMap<&str, &Schema> = BTreeMap::new();
    let mut findings = Vec::new();
    for schema in schemas {
        match first.get(schema.name.as_str()) {
            Some(earlier) => findings.push(Finding::new(
                &schema.path,
                schema.line,
                format!(
                    "`{}` takes the schema name `{}`, which a type at {}:{} takes too: the \
                     document keeps one schema of each name, so the operations of one would \
                     document the other's; name the type after its resource (API-17)",
                    schema.type_name, schema.name, earlier.path, earlier.line
                ),
            )),
            None => {
                first.insert(&schema.name, schema);
            }
        }
    }
    findings
}

/// The components of `document`'s two catalogs, `Frame` and `AuditEntry`, those that they refer
/// to and those that the document marks as a frame type's or an action's, each with whose.
fn catalogs(document: &Value) -> BTreeMap<&str, &'static str> {
    let mut components = BTreeMap::new();
    for (catalog, mark, of) in [
        (events::CATALOG, events::MARK, "event"),
        (actions::CATALOG, actions::MARK, "action"),
    ] {
        components.insert(catalog, of);
        for component in events::referred(document, catalog) {
            components.insert(component, of);
        }
        for (component, schema) in document["components"]["schemas"]
            .as_object()
            .into_iter()
            .flatten()
        {
            if schema[mark].is_string() {
                components.insert(component.as_str(), of);
            }
        }
    }
    components
}

/// Each of `schemas` whose name is that of one of `document`'s two catalogs, of a component that
/// one of them refers to, or of one that the document marks as a frame type's or an action's.
pub fn catalog_names(schemas: &[Schema], document: &Value) -> Vec<Finding> {
    let components = catalogs(document);
    schemas
        .iter()
        .filter_map(|schema| {
            let of = components.get(schema.name.as_str())?;
            Some(Finding::new(
                &schema.path,
                schema.line,
                format!(
                    "`{}` takes the schema name `{}`, which a component of the {of} catalog \
                     takes too: the document keeps one schema of each name, so it would document \
                     one of the two under it; give the type another name (API-17)",
                    schema.type_name, schema.name
                ),
            ))
        })
        .collect()
}

/// Whether `component`, a component's name, is `schema`'s, or an instance's of a generic one.
fn names(schema: &Schema, component: &str) -> bool {
    component == schema.name
        || schema.generic
            && component
                .strip_prefix(schema.name.as_str())
                .is_some_and(|rest| rest.starts_with('_'))
}

/// Each of `schemas` whose `ToSchema` a file of `app` writes, and whose name no component of
/// `document` holds.
fn unlisted(schemas: &[Schema], document: &Value) -> Vec<Finding> {
    let empty = Map::new();
    let components = document["components"]["schemas"]
        .as_object()
        .unwrap_or(&empty);
    schemas
        .iter()
        .filter(|schema| {
            schema.in_app && !components.keys().any(|component| names(schema, component))
        })
        .map(|schema| {
            Finding::new(
                &schema.path,
                schema.line,
                format!(
                    "`{}` takes the schema name `{}`, which no component of openapi.json holds: \
                     delete the `ToSchema` that no route documents, or implement `PartialSchema` \
                     alone for a type that a route only inlines (CODE-15, API-17)",
                    schema.type_name, schema.name
                ),
            )
        })
        .collect()
}

/// Each component of `document` that is none of `schemas`' names, none of the catalogs', and no
/// instance of a generic one of `schemas`.
pub fn unread(schemas: &[Schema], document: &Value) -> Vec<Problem> {
    let empty = Map::new();
    let catalogs = catalogs(document);
    document["components"]["schemas"]
        .as_object()
        .unwrap_or(&empty)
        .keys()
        .filter(|component| {
            !catalogs.contains_key(component.as_str())
                && !schemas.iter().any(|schema| names(schema, component))
        })
        .map(|component| {
            (
                quoted(component),
                format!(
                    "the component {component} is no schema name that the check reads in the \
                     code, no catalog's, and no instance of a generic type: write its type's \
                     `ToSchema` in a form that the comment of crates/xtask/src/openapi/names.rs \
                     lists (API-17)"
                ),
            )
        })
        .collect()
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

        let mut found: Vec<String> = duplicates(&code(&repo).schemas)
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

        let found: Vec<String> = catalog_names(&code(&repo).schemas, &document)
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

    #[test]
    fn code_15_an_app_to_schema_whose_name_no_component_holds_is_reported() {
        let app = "#[derive(Serialize, ToSchema)]\nstruct Listed {}\n\
                   #[derive(Serialize, ToSchema)]\nstruct Gone {}\n\
                   #[derive(Serialize, ToSchema)]\nstruct Page<T> {}\n\
                   #[derive(Serialize, ToSchema)]\nstruct Unused<T> {}\n\
                   platform::values!(\n    /// Inlined in a query.\n    Inlined(Listed)\n);\n";
        let platform = "#[derive(Serialize, ToSchema)]\npub struct Inline;\n\
                        macro_rules! values {\n    ($(#[$doc:meta])* $name:ident($item:ty)) => {\n        \
                        impl ToSchema for $name {}\n    };\n}\n";
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/model.rs", app),
            ("crates/platform/src/lists.rs", platform),
        ]);
        let document = json!({ "components": { "schemas": { "Listed": {}, "Page_Listed": {} } } });

        let found: Vec<String> = findings(code(&repo), &document)
            .into_iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();

        assert_eq!(
            found,
            [
                "crates/app/src/modules/things/model.rs:4",
                "crates/app/src/modules/things/model.rs:8",
            ]
        );
    }

    #[test]
    fn api_17_each_component_is_a_name_that_the_check_reads_a_catalog_s_or_an_instance() {
        let model = "#[derive(Serialize, ToSchema)]\nstruct Listed {}\n\
                     #[derive(Serialize, ToSchema)]\nstruct Page<T> {}\n\
                     #[derive(Serialize, ToSchema)]\nstruct Other {}\n";
        let repo = Repo::of(&[("crates/platform/src/lists.rs", model)]);
        let document = json!({ "components": { "schemas": {
            "Listed": {},
            "Other": {},
            "Page_Listed": {},
            "Other_Listed": {},
            "Pages": {},
            "Unknown": {},
            "Frame": { "oneOf": [{ "$ref": "#/components/schemas/ThingMadeFrame" }] },
            "ThingMadeFrame": {},
            "ThingKeptFrame": { "x-event": "thing.kept" },
            "AuditEntry": {},
            "ThingMadeEntry": { "x-audit-action": "thing.made" },
        } } });

        let mut unread: Vec<String> = unread(&code(&repo).schemas, &document)
            .into_iter()
            .map(|(needle, _)| needle)
            .collect();
        unread.sort();

        assert_eq!(unread, ["\"Other_Listed\"", "\"Pages\"", "\"Unknown\""]);
    }

    #[test]
    fn api_17_two_names_that_hand_written_impls_give_alike_are_reported() {
        let platform = "macro_rules! id_type {\n    ($name:ident) => {\n        \
                        impl ToSchema for $name {}\n    };\n}\nid_type!(Same);\n";
        let app = "#[derive(Serialize, ToSchema)]\nstruct Same {}\n\
                   impl<P: Kind> ToSchema for Coded<P> {\n    fn name() -> Cow<'static, str> {\n        \
                   Cow::Borrowed(P::NAME)\n    }\n}\n\
                   impl Kind for A {\n    const NAME: &'static str = \"Same\";\n}\n";
        let repo = Repo::of(&[
            ("crates/platform/src/types.rs", platform),
            ("crates/app/src/modules/things/model.rs", app),
        ]);

        let found: Vec<String> = duplicates(&code(&repo).schemas)
            .into_iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();

        assert_eq!(
            found,
            [
                "crates/app/src/modules/things/model.rs:2",
                "crates/app/src/modules/things/model.rs:9",
            ]
        );
    }
}
