//! AUDIT-012: the document's action catalog is the components that `AuditEntry` refers to. The
//! check reads the code's actions as text: the name in double quotes that follows each
//! `Action::new(` in a module's `audit.rs`. One that no such name follows fails, since the check
//! couldn't read it. A name that no component of the catalog is marked with fails, so a module
//! that declares an action and leaves it out of its list, a list that the composition root's
//! catalog leaves out, or a document written before the action existed, fails; and so does a
//! component that the document marks as an action's and that `AuditEntry` doesn't refer to.
//! AUDIT-014: each action's component gives its entries' `action` as the action's name alone.
//! AUDIT-009: each action is named `<resource>.<verb>`, each part `snake_case`, in at most 100
//! characters, as a log's filter takes it.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::{
    events::{declared, referred},
    is_snake_case,
};
use crate::{Finding, repo::Repo, text::line_of};

/// The most characters of an action's name that a log's filter takes (AUDIT-009).
const NAME_MAX_CHARS: usize = 100;

/// The constructor of a module's audit actions, which clippy allows only in its `audit.rs`.
const ACTION: &str = "Action::new(";

/// The component of the action catalog, which holds every action's (AUDIT-012).
pub(super) const CATALOG: &str = "AuditEntry";

/// The extension that marks an action's component with the action's name.
pub(super) const MARK: &str = "x-audit-action";

/// A finding for each `Action::new(` of a module's `audit.rs` that no name in double quotes
/// follows, for each name that follows one and that no component of `document`'s catalog is
/// marked with, and for each component of `document` marked as an action's that the catalog
/// doesn't refer to, whose action isn't named as a log's filter takes it, or that gives its
/// entries' `action` as anything but the action's name.
pub fn check(repo: &Repo, document: &Value) -> Vec<Finding> {
    let catalog = referred(document, CATALOG);
    let schemas = &document["components"]["schemas"];
    let actions: BTreeSet<&str> = catalog
        .iter()
        .filter_map(|component| schemas[*component][MARK].as_str())
        .collect();
    let mut findings = components(repo, document, &catalog);
    for file in repo.files().filter(|file| is_audit_file(&file.path)) {
        for (name, line) in declared(&file.text, &[ACTION]) {
            let Some(name) = name else {
                findings.push(Finding::new(
                    &file.path,
                    line,
                    "the action's name isn't a string in double quotes right after \
                     `Action::new(`, which is how `check-openapi` reads it: write it there \
                     (AUDIT-012)",
                ));
                continue;
            };
            if !actions.contains(name) {
                findings.push(Finding::new(
                    &file.path,
                    line,
                    format!(
                        "`{name}` isn't in openapi.json's action catalog: list it in the \
                         module's `ACTIONS`, which `catalog()` in the composition root lists, \
                         then run `just openapi` (AUDIT-012)"
                    ),
                ));
            }
        }
    }
    findings
}

/// A finding for each component in `document` marked as an action's that `catalog`, the
/// components that the document's catalog refers to, lacks, whose action isn't named as a log's
/// filter takes it, or that gives its entries' `action` as anything but the action's name.
fn components(repo: &Repo, document: &Value, catalog: &BTreeSet<&str>) -> Vec<Finding> {
    let Some(file) = repo.file(super::DOCUMENT) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for (component, schema) in document["components"]["schemas"]
        .as_object()
        .into_iter()
        .flatten()
    {
        let Some(name) = schema[MARK].as_str() else {
            continue;
        };
        let line = line_of(&file.text, &format!("\"{MARK}\": \"{name}\""));
        if !catalog.contains(component.as_str()) {
            findings.push(Finding::new(
                super::DOCUMENT,
                line,
                format!(
                    "the component {component} of the action {name} isn't in the action catalog, \
                     the component {CATALOG} (AUDIT-012)"
                ),
            ));
        }
        let shaped = name.len() <= NAME_MAX_CHARS
            && name
                .split_once('.')
                .is_some_and(|(resource, verb)| is_snake_case(resource) && is_snake_case(verb));
        if !shaped {
            findings.push(Finding::new(
                super::DOCUMENT,
                line,
                format!(
                    "the action {name} isn't named <resource>.<verb>, each part snake_case, in at \
                     most {NAME_MAX_CHARS} characters, as a log's filter takes it (AUDIT-009)"
                ),
            ));
        }
        let named = schema["allOf"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|part| part["properties"]["action"]["enum"] == json!([name]));
        if !named {
            findings.push(Finding::new(
                super::DOCUMENT,
                line,
                format!(
                    "the component {component} of the action {name} doesn't give its entries' \
                     `action` as `{name}` alone (AUDIT-014)"
                ),
            ));
        }
    }
    findings
}

/// Whether `path` is a module's `audit.rs`.
fn is_audit_file(path: &str) -> bool {
    path.strip_prefix("crates/app/src/modules/")
        .and_then(|rest| rest.strip_suffix("/audit.rs"))
        .is_some_and(|module| !module.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The component of the action `name`, whose entries' `action` is `action`.
    fn component(name: &str, action: &str) -> Value {
        json!({
            "allOf": [
                { "$ref": "#/components/schemas/LoggedEntry" },
                { "properties": { "action": { "enum": [action] } } },
            ],
            "x-audit-action": name,
        })
    }

    /// The catalog's component, which holds the components `names`.
    fn catalog_of(names: &[&str]) -> Value {
        let entries: Vec<Value> = names
            .iter()
            .map(|name| json!({ "$ref": format!("#/components/schemas/{name}") }))
            .collect();
        json!({ "oneOf": entries })
    }

    #[test]
    fn audit_014_an_action_s_component_that_names_another_action_or_an_unshaped_name_fails() {
        let longest = format!("{}.made", "a".repeat(95));
        let longer = format!("{}.made", "a".repeat(96));
        let document = json!({ "components": { "schemas": {
            "A": component("thing.made", "thing.made"),
            "B": component("sign_in.failed", "sign_in.failed"),
            "C": component("thing.gone", "thing.made"),
            "D": component("thingMade", "thingMade"),
            "E": component(&longest, &longest),
            "F": component(&longer, &longer),
            "AuditEntry": catalog_of(&["A", "B", "C", "D", "E", "F"]),
        } } });
        let repo = Repo::of(&[("openapi.json", &document.to_string())]);

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| finding.message)
            .collect();

        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(messages[0].starts_with("the component C of the action thing.gone"));
        assert!(messages[0].ends_with("(AUDIT-014)"));
        assert!(messages[1].starts_with("the action thingMade isn't named"));
        assert!(messages[2].starts_with(&format!("the action {longer} isn't named")));
        assert!(
            messages[1..]
                .iter()
                .all(|message| message.ends_with("(AUDIT-009)"))
        );
    }

    #[test]
    fn audit_012_a_component_of_an_action_that_the_catalog_lacks_fails() {
        let audit = "const A: Action = Action::new(\"thing.made\", \"Made.\", &[THING]);\n\
                     const B: Action = Action::new(\"thing.kept\", \"Kept.\", &[THING]);\n";
        let document = json!({ "components": { "schemas": {
            "ThingMadeEntry": component("thing.made", "thing.made"),
            "ThingKeptEntry": component("thing.kept", "thing.kept"),
            "AuditEntry": catalog_of(&["ThingMadeEntry"]),
        } } });
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/audit.rs", audit),
            ("openapi.json", &document.to_string()),
        ]);

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}: {}", finding.path, finding.message))
            .collect();

        // The component, and the declaration whose action the catalog so lacks.
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].starts_with(
                "openapi.json: the component ThingKeptEntry of the action thing.kept isn't in \
                 the action catalog"
            ),
            "{messages:?}"
        );
        assert!(
            messages[1].starts_with("crates/app/src/modules/things/audit.rs: `thing.kept`"),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .all(|message| message.ends_with("(AUDIT-012)"))
        );
    }

    #[test]
    fn audit_012_a_component_that_the_catalog_lacks_fails_though_the_catalog_holds_its_action() {
        let audit = "const A: Action = Action::new(\"thing.made\", \"Made.\", &[THING]);\n";
        let document = json!({ "components": { "schemas": {
            "ThingMadeEntry": component("thing.made", "thing.made"),
            "ThingMadeAgainEntry": component("thing.made", "thing.made"),
            "AuditEntry": catalog_of(&["ThingMadeEntry"]),
        } } });
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/audit.rs", audit),
            ("openapi.json", &document.to_string()),
        ]);

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}: {}", finding.path, finding.message))
            .collect();

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].starts_with(
                "openapi.json: the component ThingMadeAgainEntry of the action thing.made isn't \
                 in the action catalog"
            ),
            "{messages:?}"
        );
    }

    #[test]
    fn audit_012_a_document_without_the_catalog_fails_on_each_action_and_each_component() {
        let audit = "const A: Action = Action::new(\"thing.made\", \"Made.\", &[THING]);\n";
        let document = json!({ "components": { "schemas": {
            "ThingMadeEntry": component("thing.made", "thing.made"),
        } } });
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/audit.rs", audit),
            ("openapi.json", &document.to_string()),
        ]);

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}: {}", finding.path, finding.message))
            .collect();

        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].starts_with("openapi.json: the component ThingMadeEntry"));
        assert!(messages[1].starts_with("crates/app/src/modules/things/audit.rs: `thing.made`"));
        // A component of the catalog that another action marks leaves the action out of it.
        let document = json!({ "components": { "schemas": {
            "ThingMadeEntry": component("thing.gone", "thing.gone"),
            "AuditEntry": catalog_of(&["ThingMadeEntry"]),
        } } });
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/audit.rs", audit),
            ("openapi.json", &document.to_string()),
        ]);
        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}: {}", finding.path, finding.message))
            .collect();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].starts_with("crates/app/src/modules/things/audit.rs: `thing.made`"));
    }

    #[test]
    fn audit_012_an_action_s_constructor_that_no_quoted_name_follows_fails() {
        // A raw string and a constant, with a name the check reads after them.
        let audit = "const A: Action = Action::new(r\"thing.made\", \"Made.\", &[THING]);\n\
                     const B: Action = Action::new(NAME, \"Kept.\", &[THING]);\n\
                     const C: Action = Action::new(\"thing.gone\", \"Gone.\", &[THING]);\n";
        let repo = Repo::of(&[("crates/app/src/modules/things/audit.rs", audit)]);
        let document = json!({ "components": { "schemas": {
            "ThingGoneEntry": { "x-audit-action": "thing.gone" },
            "AuditEntry": catalog_of(&["ThingGoneEntry"]),
        } } });

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
            .collect();

        assert_eq!(messages.len(), 2, "{messages:?}");
        for (message, line) in messages.iter().zip([1, 2]) {
            assert!(
                message.starts_with(&format!(
                    "crates/app/src/modules/things/audit.rs:{line}: the action's name isn't a \
                     string in double quotes"
                )),
                "{messages:?}"
            );
            assert!(message.ends_with("(AUDIT-012)"), "{messages:?}");
        }
    }

    #[test]
    fn audit_012_an_action_that_a_module_declares_and_the_catalog_lacks_fails() {
        let audit = "const A: Action = Action::new(\"thing.made\", \"Made.\", &[THING]);\n\
                     const B: Action<Gone> = Action::new(\n    \"thing.gone\",\n    \"Gone.\",\n    \
                     &[THING],\n);\n";
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/audit.rs", audit),
            (
                "crates/app/src/modules/audit/action.rs",
                "let own: Action = Action::new(\"test.only\", \"A test's.\", &[USER]);",
            ),
        ]);
        let document = json!({ "components": { "schemas": {
            "ThingMadeEntry": { "x-audit-action": "thing.made" },
            "AuditEntry": catalog_of(&["ThingMadeEntry"]),
        } } });

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
            .collect();

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].starts_with("crates/app/src/modules/things/audit.rs:3: `thing.gone`"),
            "{messages:?}"
        );
    }
}
