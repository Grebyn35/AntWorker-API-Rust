//! REALTIME-008: every event that a module's `events.rs` declares is in the document's event
//! catalog, so a module that declares an event and forgets to register it with its routes, or a
//! document written before the event existed, fails. API-19: each frame type in the catalog is
//! named `<resource>.<verb>`, each part `snake_case`; review checks that the verb is in the past
//! tense.

use std::collections::BTreeSet;

use serde_json::Value;

use super::is_snake_case;
use crate::{Finding, repo::Repo, text::line_of};

/// The constructors of a module's events, which clippy allows only in its `events.rs`.
const CONSTRUCTORS: [&str; 2] = ["UserEvent::new(", "WorkspaceEvent::new("];

/// A finding for each event that a module's `events.rs` declares and `document`'s catalog lacks.
pub fn check(repo: &Repo, document: &Value) -> Vec<Finding> {
    let catalog: BTreeSet<&str> = document["components"]["schemas"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, schema)| schema["x-event"].as_str())
        .collect();
    let mut findings = Vec::new();
    if let Some(file) = repo.file(super::DOCUMENT) {
        for name in &catalog {
            let shaped = name
                .split_once('.')
                .is_some_and(|(resource, verb)| is_snake_case(resource) && is_snake_case(verb));
            if !shaped {
                findings.push(Finding::new(
                    super::DOCUMENT,
                    line_of(&file.text, &format!("\"x-event\": \"{name}\"")),
                    format!(
                        "the frame type {name} isn't named <resource>.<verb>, each part \
                         snake_case (API-19)"
                    ),
                ));
            }
        }
    }
    for file in repo.files().filter(|file| is_events_file(&file.path)) {
        for name in declared(&file.text) {
            if !catalog.contains(name) {
                findings.push(Finding::new(
                    &file.path,
                    line_of(&file.text, &format!("\"{name}\"")),
                    format!(
                        "`{name}` isn't in openapi.json's event catalog: register it with the \
                         module's routes through `Routes::events`, then run `just openapi` \
                         (REALTIME-008)"
                    ),
                ));
            }
        }
    }
    findings
}

/// Whether `path` is a module's `events.rs`.
fn is_events_file(path: &str) -> bool {
    path.strip_prefix("crates/app/src/modules/")
        .and_then(|rest| rest.strip_suffix("/events.rs"))
        .is_some_and(|module| !module.contains('/'))
}

/// The name of each event that `text` declares: the string that each constructor takes first.
fn declared(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    for constructor in CONSTRUCTORS {
        for (at, _) in text.match_indices(constructor) {
            let rest = text[at + constructor.len()..].trim_start();
            if let Some(name) = rest
                .strip_prefix('"')
                .and_then(|quoted| quoted.split_once('"'))
                .map(|(name, _)| name)
            {
                names.push(name);
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn realtime_008_an_event_that_a_module_declares_and_the_catalog_lacks_fails() {
        let events = "const A: UserEvent<A> = UserEvent::new(\"thing.made\");\n\
                      const B: WorkspaceEvent<P, B> =\n    WorkspaceEvent::new(\n        \"thing.gone\",\n    );\n";
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/events.rs", events),
            (
                "crates/app/tests/integration/support/fixtures.rs",
                "const C: UserEvent<C> = UserEvent::new(\"test.only\");",
            ),
        ]);
        let document = json!({ "components": { "schemas": {
            "ThingMadeFrame": { "x-event": "thing.made" },
        } } });

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
            .collect();

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].starts_with("crates/app/src/modules/things/events.rs:4: `thing.gone`"),
            "{messages:?}"
        );
    }

    #[test]
    fn api_19_a_frame_type_that_isn_t_resource_dot_verb_in_snake_case_fails() {
        let document = json!({ "components": { "schemas": {
            "A": { "x-event": "member.removed" },
            "B": { "x-event": "sign_in.failed" },
            "C": { "x-event": "memberRemoved" },
            "D": { "x-event": "Member.removed" },
        } } });
        let repo = Repo::of(&[("openapi.json", &document.to_string())]);

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| finding.message)
            .collect();

        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages.iter().all(|message| message.ends_with("(API-19)")));
    }
}
