//! The action catalog (the audit spec's "The action catalog").

use std::collections::BTreeSet;

use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::support::{Api, users, workspaces};

/// What the catalog states of one action, as its schema in the document gives it.
struct Stated {
    /// The action's name.
    action: String,
    /// The sentence that tells what it means.
    meaning: String,
    /// The logs that record it: `user`, a user's own, and `workspace`, a workspace's.
    logs: Vec<&'static str>,
    /// The kinds of resource that its entries happen to.
    resource_types: Vec<String>,
    /// The permission that reading it in a workspace's log takes beside `audit.read`, if any.
    permission: Option<String>,
    /// Its schema.
    schema: Value,
}

/// The document's schema of the component that `reference` names.
fn referred<'a>(document: &'a Value, reference: &Value) -> &'a Value {
    let name = reference.as_str().unwrap().rsplit('/').next().unwrap();
    &document["components"]["schemas"][name]
}

/// What `schema`, an action's, states of it. Its description is the action's name, the sentence
/// that tells what it means, the logs that record it, and who in a workspace's log reads it
/// (AUDIT-014).
fn stated(schema: &Value) -> Stated {
    let action = schema["x-audit-action"].as_str().unwrap().to_owned();
    let description = schema["description"].as_str().unwrap();
    let told = description.strip_prefix(&format!("`{action}`: "));
    assert!(told.is_some(), "{description}");
    let parts = told.unwrap().rsplit_once(" It is recorded in ");
    assert!(parts.is_some(), "{description}");
    let (meaning, rest) = parts.unwrap();
    let (recorded, readers) = rest.split_once('.').unwrap();
    let logs: Vec<&'static str> = recorded
        .split(" and ")
        .map(|log| match log {
            "a user's own log" => "user",
            "a workspace's log" => "workspace",
            _ => "",
        })
        .collect();
    assert!(!logs.contains(&""), "{description}");
    let permission = match readers.trim() {
        "" | "A workspace's log shows it to whoever reads the log." => None,
        shown => {
            let held = shown
                .strip_prefix("A workspace's log shows it to a reader whose role holds `")
                .and_then(|rest| rest.strip_suffix("`."));
            assert!(held.is_some(), "{description}");
            held.map(str::to_owned)
        }
    };
    // Only an action that a workspace's log records says who reads it there.
    assert_eq!(
        readers.is_empty(),
        !logs.contains(&"workspace"),
        "{description}"
    );
    let resource_types = schema["allOf"][1]["properties"]["resource_type"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap().to_owned())
        .collect();
    Stated {
        action,
        meaning: meaning.to_owned(),
        logs,
        resource_types,
        permission,
        schema: schema.clone(),
    }
}

/// The catalog of `api`'s document: each action that the component `AuditEntry` holds, in its
/// order, as its schema states it (AUDIT-012).
fn catalog(api: &Api) -> Vec<Stated> {
    let document = api.document();
    document["components"]["schemas"]["AuditEntry"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| stated(referred(document, &entry["$ref"])))
        .collect()
}

/// The catalog's action named `name`.
fn named<'a>(actions: &'a [Stated], name: &str) -> &'a Stated {
    let found = actions.iter().find(|stated| stated.action == name);
    assert!(found.is_some(), "{name} isn't in the catalog");
    found.unwrap()
}

#[tokio::test]
async fn audit_012_the_catalog_holds_each_action_once_in_the_order_of_their_names() {
    let api = Api::new().await;
    let document = api.document();

    let actions = catalog(&api);

    let names: Vec<&str> = actions
        .iter()
        .map(|stated| stated.action.as_str())
        .collect();
    let ordered: Vec<&str> = names
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(
        names, ordered,
        "the actions are in the order of their names, each once"
    );
    // Every schema that the document marks as an action's is in the catalog.
    let marked: BTreeSet<&str> = document["components"]["schemas"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(|schema| schema["x-audit-action"].as_str())
        .collect();
    assert_eq!(marked, names.iter().copied().collect::<BTreeSet<_>>());
    for name in ["access.denied", "member.role_changed", "user.registered"] {
        assert!(names.contains(&name), "{name}");
    }
    assert_eq!(
        document["components"]["schemas"]["AuditEntry"]["description"],
        "An entry of an audit log, one schema for each action, which its `action` names \
         (AUDIT-012)."
    );
}

#[tokio::test]
async fn audit_012_each_name_in_the_catalog_is_one_a_log_s_filter_takes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let actions = catalog(&api);
    let mut resources = BTreeSet::new();
    for stated in &actions {
        resources.extend(stated.resource_types.iter().cloned());
    }
    let names: Vec<String> = actions.iter().map(|stated| stated.action.clone()).collect();

    // A filter takes at most 50 values (AUDIT-009).
    for (parameter, values) in [
        ("action", names),
        ("resource_type", resources.into_iter().collect()),
    ] {
        assert!(!values.is_empty(), "{parameter}");
        for chunk in values.chunks(50) {
            let path = format!("/v1/users/me/audit-entries?{parameter}={}", chunk.join(","));
            let response = api.get(&path).bearer(&ada.access).send().await;
            assert_eq!(response.status, StatusCode::OK, "{path}");
        }
    }
}

/// Has `api`'s users make changes that record entries of both kinds of log, each sending what a
/// client sends: a registration, a failed sign-in, a member's rename, role change and removal,
/// a member's refusal, and a non-member's.
async fn change(api: &Api) {
    let olivia = users::register(api, "olivia@example.com").await;
    let mia = users::register(api, "mia@example.com").await;
    let stranger = users::register(api, "stranger@example.com").await;
    users::sign_in(api, "mia@example.com", "wrong-password-123").await;
    let members = [(olivia.user_id.as_str(), "owner"), (&mia.user_id, "admin")];
    let acme = workspaces::arrange(api.database(), "Acme", &members).await;
    let workspace = format!("/v1/workspaces/{acme}");
    let member = format!("{workspace}/members/{}", mia.user_id);
    let rename = json!({ "name": "Acme Two" });
    let demote = json!({ "role": "member" });
    for (caller, method, path, body, status) in [
        (
            &olivia,
            Method::PATCH,
            &workspace,
            Some(&rename),
            StatusCode::OK,
        ),
        (
            &olivia,
            Method::PATCH,
            &member,
            Some(&demote),
            StatusCode::OK,
        ),
        // A member's refusal goes to the workspace's log, and a non-member's to their own
        // (AUDIT-004).
        (
            &mia,
            Method::PATCH,
            &workspace,
            Some(&rename),
            StatusCode::FORBIDDEN,
        ),
        (
            &stranger,
            Method::GET,
            &workspace,
            None,
            StatusCode::NOT_FOUND,
        ),
        (
            &olivia,
            Method::DELETE,
            &member,
            None,
            StatusCode::NO_CONTENT,
        ),
    ] {
        let call = match body {
            Some(body) => api.patch(path, body),
            None => api.request(method.clone(), path),
        };
        let response = call.bearer(&caller.access).send().await;
        assert_eq!(response.status, status, "{method} {path}");
    }
}

#[tokio::test]
async fn audit_013_each_entry_that_changes_record_is_in_a_log_and_of_a_resource_its_action_declares()
 {
    let api = Api::new().await;
    change(&api).await;
    let actions = catalog(&api);

    let entries = api
        .database()
        .owner_reads(
            "SELECT action || ' ' || scope || ' ' || resource_type || ' ' \
             || coalesce(permission, '-') FROM audit_entries ORDER BY created_at, id",
        )
        .await;

    let mut recorded = BTreeSet::new();
    for entry in &entries {
        let words: Vec<&str> = entry.split(' ').collect();
        assert_eq!(words.len(), 4, "{entry}");
        let (action, log, resource, permission) = (words[0], words[1], words[2], words[3]);
        let declared = named(&actions, action);
        assert!(declared.logs.contains(&log), "{entry}");
        assert!(
            declared.resource_types.iter().any(|kind| kind == resource),
            "{entry}"
        );
        if log == "workspace" {
            let expected = declared.permission.as_deref().unwrap_or("-");
            assert_eq!(expected, permission, "{entry}");
        }
        recorded.insert(format!("{action} {log}"));
    }
    for expected in [
        "user.registered user",
        "sign_in.failed user",
        "workspace.renamed workspace",
        "member.role_changed workspace",
        "access.denied workspace",
        "access.denied user",
        "member.removed workspace",
    ] {
        assert!(recorded.contains(expected), "{expected} in {entries:?}");
    }
}

#[tokio::test]
async fn audit_014_the_document_documents_each_action_of_the_catalog_with_its_details() {
    let api = Api::new().await;
    let actions = catalog(&api);
    let document = api.document();
    let details = |stated: &Stated| {
        referred(
            document,
            &stated.schema["allOf"][1]["properties"]["details"]["$ref"],
        )
    };

    assert!(!actions.is_empty());
    for stated in &actions {
        let name = stated.action.as_str();
        assert_eq!(
            stated.schema["allOf"][0],
            json!({ "$ref": "#/components/schemas/LoggedEntry" }),
            "{name}"
        );
        let properties = &stated.schema["allOf"][1]["properties"];
        assert_eq!(properties["action"]["enum"], json!([name]));
        assert!(stated.meaning.ends_with('.'), "{name}");
        assert!(!stated.logs.is_empty(), "{name}");
        assert!(!stated.resource_types.is_empty(), "{name}");
        if !stated.logs.contains(&"workspace") {
            assert_eq!(stated.permission, None, "{name}");
        }
        assert_eq!(details(stated)["type"], "object", "{name}");
    }
    // Each description holds the action's, the logs that record it, and who in a workspace's
    // log reads it.
    let described = |name: &str| {
        named(&actions, name).schema["description"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        described("member.role_changed"),
        "`member.role_changed`: A member changed the role of another. It is recorded in a \
         workspace's log. A workspace's log shows it to a reader whose role holds `members.read`."
    );
    assert_eq!(
        described("user.registered"),
        "`user.registered`: The user registered an account. It is recorded in a user's own log."
    );
    assert_eq!(
        described("access.denied"),
        "`access.denied`: A request was refused, since the caller may not do what it asks. It is \
         recorded in a user's own log and a workspace's log. A workspace's log shows it to \
         whoever reads the log."
    );
    let changed = named(&actions, "member.role_changed");
    assert_eq!(changed.logs, ["workspace"]);
    assert_eq!(changed.resource_types, ["workspace"]);
    assert_eq!(changed.permission.as_deref(), Some("members.read"));
    let registered = named(&actions, "user.registered");
    assert_eq!(registered.logs, ["user"]);
    assert_eq!(registered.resource_types, ["user"]);
    assert_eq!(registered.permission, None);
    // A refusal is in both logs, and every reader of a workspace's log reads it (AUDIT-004).
    let denied = named(&actions, "access.denied");
    assert_eq!(denied.logs, ["user", "workspace"]);
    assert_eq!(denied.permission, None);
    assert!(denied.resource_types.iter().any(|kind| kind == "workspace"));
    let role_change = details(changed);
    assert_eq!(role_change["required"], json!(["from", "to"]));
    assert_eq!(
        role_change["properties"]["from"]["$ref"],
        "#/components/schemas/Role"
    );
    assert!(details(denied)["properties"].get("permission").is_some());
    assert_eq!(
        details(named(&actions, "user.renamed"))["properties"],
        Value::Null
    );
}
