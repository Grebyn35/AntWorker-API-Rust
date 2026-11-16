//! AUDIT-014: every audit action that a module's `audit.rs` declares is in the document's action
//! catalog, so a module that declares an action and leaves it out of its list, a list that the
//! composition root's catalog leaves out, or a document written before the action existed,
//! fails.

use std::collections::BTreeSet;

use serde_json::Value;

use super::events::declared;
use crate::{Finding, repo::Repo, text::line_of};

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
    let mut findings = Vec::new();
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

/// Whether `path` is a module's `audit.rs`.
fn is_audit_file(path: &str) -> bool {
    path.strip_prefix("crates/app/src/modules/")
        .and_then(|rest| rest.strip_suffix("/audit.rs"))
        .is_some_and(|module| !module.contains('/'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

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
