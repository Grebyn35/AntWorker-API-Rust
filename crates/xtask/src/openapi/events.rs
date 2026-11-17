//! REALTIME-008: the document's event catalog is the components that `Frame` refers to. A frame
//! type that the code declares and that no component of the catalog is marked with fails, so a
//! module that declares an event and forgets to register it with its routes, or a document
//! written before the event existed, fails; so does a component that the document marks as a
//! frame type's and that `Frame` doesn't refer to; and so does a name that two declarations give,
//! since the catalog keeps one frame type of each name. The declarations it reads are each
//! string that follows `UserEvent::new(` or `WorkspaceEvent::new(` in a module's `events.rs`, or
//! `StreamFrame::new(` in a file of a module.
//! API-19: each frame type that the document marks is named `<resource>.<verb>`, each part
//! `snake_case`; review checks that the verb is in the past tense.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::is_snake_case;
use crate::{Finding, repo::Repo, text::line_of};

/// The component of the event catalog, which holds every frame type's (REALTIME-008).
pub(super) const CATALOG: &str = "Frame";

/// The extension that marks a frame type's component with the type's name.
pub(super) const MARK: &str = "x-event";

/// The directory of the modules' code.
const MODULES: &str = "crates/app/src/modules/";

/// The constructor of a module's user events, which clippy allows only in its `events.rs`.
const USER_EVENT: &str = "UserEvent::new(";

/// The constructor of a module's workspace events, allowed as [`USER_EVENT`] is.
const WORKSPACE_EVENT: &str = "WorkspaceEvent::new(";

/// The constructor of a frame of the stream's own, such as `stream.opened`.
const STREAM_FRAME: &str = "StreamFrame::new(";

/// The name of each component that `catalog`, the component of `document` that holds a catalog,
/// refers to in its `oneOf`.
pub(super) fn referred<'a>(document: &'a Value, catalog: &str) -> BTreeSet<&'a str> {
    document["components"]["schemas"][catalog]["oneOf"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["$ref"].as_str())
        .filter_map(|reference| reference.rsplit('/').next())
        .collect()
}

/// A finding for each frame type that a module declares and that no component of `document`'s
/// catalog is marked with, for each declaration that gives a name an earlier one gives, and for
/// each component of `document` marked as a frame type's that the catalog doesn't refer to, or
/// whose frame type isn't named as API-19 names one.
pub fn check(repo: &Repo, document: &Value) -> Vec<Finding> {
    let catalog = referred(document, CATALOG);
    let schemas = &document["components"]["schemas"];
    let frames: BTreeSet<&str> = catalog
        .iter()
        .filter_map(|component| schemas[*component][MARK].as_str())
        .collect();
    let mut findings = components(repo, document, &catalog);
    let mut first: BTreeMap<&str, (&str, usize)> = BTreeMap::new();
    let modules = repo
        .files()
        .filter(|file| file.path.starts_with(MODULES) && file.path.ends_with(".rs"));
    for file in modules {
        let constructors: &[&str] = if is_events_file(&file.path) {
            &[USER_EVENT, WORKSPACE_EVENT, STREAM_FRAME]
        } else {
            &[STREAM_FRAME]
        };
        for (name, line) in declared(&file.text, constructors) {
            if !frames.contains(name) {
                findings.push(Finding::new(
                    &file.path,
                    line,
                    format!(
                        "`{name}` isn't in openapi.json's event catalog: register it with the \
                         module's routes through `Routes::events`, then run `just openapi` \
                         (REALTIME-008)"
                    ),
                ));
            }
            match first.get(name) {
                Some((path, at)) => findings.push(Finding::new(
                    &file.path,
                    line,
                    format!(
                        "`{name}` is declared at {path}:{at} too, and the event catalog keeps \
                         one frame type of each name, so it would document one of the two: give \
                         each event, and each frame of the stream's own, a name of its own \
                         (REALTIME-008)"
                    ),
                )),
                None => {
                    first.insert(name, (&file.path, line));
                }
            }
        }
    }
    findings
}

/// A finding for each component in `document` marked as a frame type's that `catalog`, the
/// components that the document's catalog refers to, lacks, or whose frame type isn't named
/// `<resource>.<verb>`, each part `snake_case`.
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
                    "the component {component} of the frame type {name} isn't in the event \
                     catalog, the component {CATALOG} (REALTIME-008)"
                ),
            ));
        }
        let shaped = name
            .split_once('.')
            .is_some_and(|(resource, verb)| is_snake_case(resource) && is_snake_case(verb));
        if !shaped {
            findings.push(Finding::new(
                super::DOCUMENT,
                line,
                format!(
                    "the frame type {name} isn't named <resource>.<verb>, each part snake_case \
                     (API-19)"
                ),
            ));
        }
    }
    findings
}

/// Whether `path` is a module's `events.rs`.
pub fn is_events_file(path: &str) -> bool {
    path.strip_prefix(MODULES)
        .and_then(|rest| rest.strip_suffix("/events.rs"))
        .is_some_and(|module| !module.contains('/'))
}

/// The name of each workspace event that `text`, a module's `events.rs`, declares.
pub fn workspace_events(text: &str) -> Vec<&str> {
    declared(text, &[WORKSPACE_EVENT])
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// The name of each event, frame of the stream's own or audit action that `text` declares
/// through one of `constructors`: each string that follows one of them, with the string's line,
/// in the order of the lines.
pub(super) fn declared<'a>(text: &'a str, constructors: &[&str]) -> Vec<(&'a str, usize)> {
    let mut found = Vec::new();
    for constructor in constructors {
        for (at, _) in text.match_indices(constructor) {
            let arguments = &text[at + constructor.len()..];
            let first = arguments.trim_start();
            if let Some((name, _)) = first
                .strip_prefix('"')
                .and_then(|quoted| quoted.split_once('"'))
            {
                found.push((text.len() - first.len(), name));
            }
        }
    }
    found.sort_unstable();
    found
        .into_iter()
        .map(|(at, name)| (name, text[..at].matches('\n').count() + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// The catalog's component, which holds the components `names`.
    fn catalog_of(names: &[&str]) -> Value {
        let entries: Vec<Value> = names
            .iter()
            .map(|name| json!({ "$ref": format!("#/components/schemas/{name}") }))
            .collect();
        json!({ "oneOf": entries })
    }

    /// What `check` finds in `repo` and `document`, each as `path:line: message`.
    fn found(repo: &Repo, document: &Value) -> Vec<String> {
        check(repo, document)
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
            .collect()
    }

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
            "Frame": catalog_of(&["ThingMadeFrame"]),
        } } });

        let messages = found(&repo, &document);

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].starts_with("crates/app/src/modules/things/events.rs:4: `thing.gone`"),
            "{messages:?}"
        );
    }

    #[test]
    fn realtime_008_a_frame_of_the_stream_s_own_that_the_catalog_lacks_fails() {
        let routes = "const OPENED: StreamFrame<Opened> = StreamFrame::new(\n    \
                      \"stream.opened\",\n    FrameId::Sometimes,\n    \"Opened.\",\n);\n\
                      const CLOSED: StreamFrame<Closed> =\n    \
                      StreamFrame::new(\"stream.closed\", FrameId::Never, \"Closed.\");\n";
        let repo = Repo::of(&[("crates/app/src/modules/realtime/routes.rs", routes)]);
        let document = json!({ "components": { "schemas": {
            "StreamOpenedFrame": { "x-event": "stream.opened" },
            "Frame": catalog_of(&["StreamOpenedFrame"]),
        } } });

        let messages = found(&repo, &document);

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].starts_with("crates/app/src/modules/realtime/routes.rs:7: `stream.closed`"),
            "{messages:?}"
        );
    }

    #[test]
    fn realtime_008_a_component_of_a_frame_type_that_the_catalog_lacks_fails() {
        let events = "const A: UserEvent<A> = UserEvent::new(\"thing.made\");\n\
                      const B: UserEvent<B> = UserEvent::new(\"thing.kept\");\n";
        let document = json!({ "components": { "schemas": {
            "ThingMadeFrame": { "x-event": "thing.made" },
            "ThingKeptFrame": { "x-event": "thing.kept" },
            "Frame": catalog_of(&["ThingMadeFrame"]),
        } } });
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/events.rs", events),
            ("openapi.json", &document.to_string()),
        ]);

        let messages = found(&repo, &document);

        // The component, and the declaration whose frame type the catalog so lacks.
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].starts_with(
                "openapi.json:1: the component ThingKeptFrame of the frame type thing.kept \
                 isn't in the event catalog"
            ),
            "{messages:?}"
        );
        assert!(
            messages[1].starts_with("crates/app/src/modules/things/events.rs:2: `thing.kept`"),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .all(|message| message.ends_with("(REALTIME-008)"))
        );
    }

    #[test]
    fn realtime_008_a_document_without_the_catalog_fails_on_each_declaration_and_each_component() {
        let events = "const A: UserEvent<A> = UserEvent::new(\"thing.made\");\n";
        let document = json!({ "components": { "schemas": {
            "ThingMadeFrame": { "x-event": "thing.made" },
        } } });
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/events.rs", events),
            ("openapi.json", &document.to_string()),
        ]);

        let messages = found(&repo, &document);

        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].starts_with("openapi.json:1: the component ThingMadeFrame"));
        assert!(messages[1].starts_with("crates/app/src/modules/things/events.rs:1: `thing.made`"));
    }

    #[test]
    fn realtime_008_a_name_that_two_declarations_give_fails() {
        // A user event and a workspace event of two modules, an event that takes the name of a
        // frame of the stream's own, and two events of one file.
        let things = "const A: UserEvent<A> = UserEvent::new(\"thing.made\");\n\
                      const B: UserEvent<B> = UserEvent::new(\"thing.kept\");\n\
                      const C: UserEvent<C> = UserEvent::new(\"thing.kept\");\n\
                      const D: UserEvent<D> = UserEvent::new(\"stream.opened\");\n";
        let orders = "const A: WorkspaceEvent<P, A> = WorkspaceEvent::new(\"thing.made\");\n";
        let routes = "const OPENED: StreamFrame<Opened> =\n    \
                      StreamFrame::new(\"stream.opened\", FrameId::Sometimes, \"Opened.\");\n";
        let repo = Repo::of(&[
            ("crates/app/src/modules/orders/events.rs", orders),
            ("crates/app/src/modules/realtime/routes.rs", routes),
            ("crates/app/src/modules/things/events.rs", things),
        ]);
        let document = json!({ "components": { "schemas": {
            "ThingMadeFrame": { "x-event": "thing.made" },
            "ThingKeptFrame": { "x-event": "thing.kept" },
            "StreamOpenedFrame": { "x-event": "stream.opened" },
            "Frame": catalog_of(&["ThingMadeFrame", "ThingKeptFrame", "StreamOpenedFrame"]),
        } } });

        let messages = found(&repo, &document);

        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(
            messages[0].starts_with(
                "crates/app/src/modules/things/events.rs:1: `thing.made` is declared at \
                 crates/app/src/modules/orders/events.rs:1 too"
            ),
            "{messages:?}"
        );
        assert!(
            messages[1].starts_with(
                "crates/app/src/modules/things/events.rs:3: `thing.kept` is declared at \
                 crates/app/src/modules/things/events.rs:2 too"
            ),
            "{messages:?}"
        );
        assert!(
            messages[2].starts_with(
                "crates/app/src/modules/things/events.rs:4: `stream.opened` is declared at \
                 crates/app/src/modules/realtime/routes.rs:2 too"
            ),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .all(|message| message.ends_with("(REALTIME-008)"))
        );
    }

    #[test]
    fn api_19_a_frame_type_that_isn_t_resource_dot_verb_in_snake_case_fails() {
        let document = json!({ "components": { "schemas": {
            "A": { "x-event": "member.removed" },
            "B": { "x-event": "sign_in.failed" },
            "C": { "x-event": "memberRemoved" },
            "D": { "x-event": "Member.removed" },
            "Frame": catalog_of(&["A", "B", "C", "D"]),
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
