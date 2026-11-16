//! AUDIT-014: every audit action that a module's `audit.rs` declares is in the document's action
//! catalog, so a module that declares an action and leaves it out of its list, a list that the
//! composition root's catalog leaves out, or a document written before the action existed,
//! fails; and each action's component gives its entries' `action` as the action's name alone.
//! AUDIT-009: each action is named `<resource>.<verb>`, each part `snake_case`, in at most 100
//! characters, as a log's filter takes it.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::{events::declared, is_snake_case};
use crate::{Finding, repo::Repo, text::line_of};

/// The most characters of an action's name that a log's filter takes (AUDIT-009).
const NAME_MAX_CHARS: usize = 100;

/// The constructor of a module's audit actions, which clippy allows only in its `audit.rs`.
const ACTION: &str = "Action::new(";

/// A finding for each action that a module's `audit.rs` declares and `document`'s catalog lacks.
pub fn check(repo: &Repo, document: &Value) -> Vec<Finding> {
    let catalog: BTreeSet<&str> = document["components"]["schemas"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, schema)| schema["x-audit-action"].as_str())
        .collect();
    let mut findings = components(repo, document);
    for file in repo.files().filter(|file| is_audit_file(&file.path)) {
        for name in declared(&file.text, &[ACTION]) {
            if !catalog.contains(name) {
                findings.push(Finding::new(
                    &file.path,
                    line_of(&file.text, &format!("\"{name}\"")),
                    format!(
                        "`{name}` isn't in openapi.json's action catalog: list it in the \
                         module's `ACTIONS`, which `catalog()` in the composition root lists, \
                         then run `just openapi` (AUDIT-014)"
                    ),
                ));
            }
        }
    }
    findings
}

/// A finding for each action's component in `document` whose action isn't named as a log's
/// filter takes it, or that gives its entries' `action` as anything but the action's name.
fn components(repo: &Repo, document: &Value) -> Vec<Finding> {
    let Some(file) = repo.file(super::DOCUMENT) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for (component, schema) in document["components"]["schemas"]
        .as_object()
        .into_iter()
        .flatten()
    {
        let Some(name) = schema["x-audit-action"].as_str() else {
            continue;
        };
        let line = line_of(&file.text, &format!("\"x-audit-action\": \"{name}\""));
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
    fn audit_014_an_action_that_a_module_declares_and_the_catalog_lacks_fails() {
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
