//! The action catalog (the audit spec's "The action catalog").

use std::collections::BTreeSet;

use http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::support::{Api, users, workspaces};

/// The catalog's actions, as `GET /v1/audit-catalog` answers them to `api`'s caller, who sends
/// no credential.
async fn catalog(api: &Api) -> Vec<Value> {
    let response = api.get("/v1/audit-catalog").send().await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("content-type"), "application/json");
    response.json()["actions"].as_array().unwrap().clone()
}

/// The catalog's action named `name`.
fn named<'a>(actions: &'a [Value], name: &str) -> &'a Value {
    let found = actions.iter().find(|action| action["action"] == name);
    assert!(found.is_some(), "{name} isn't in the catalog");
    found.unwrap()
}

#[tokio::test]
async fn audit_012_the_catalog_lists_each_action_with_its_meaning_logs_resources_and_permission() {
    let api = Api::new().await;

    let actions = catalog(&api).await;

    let names: Vec<&str> = actions
        .iter()
        .map(|action| action["action"].as_str().unwrap())
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
    for action in &actions {
        let description = action["description"].as_str().unwrap();
        assert!(description.ends_with('.'), "{action}");
        let logs = action["logs"].as_array().unwrap();
        assert!(!logs.is_empty(), "{action}");
        assert!(
            logs.iter().all(|log| log == "user" || log == "workspace"),
            "{action}"
        );
        assert!(
            !action["resource_types"].as_array().unwrap().is_empty(),
            "{action}"
        );
        if !logs.contains(&json!("workspace")) {
            assert_eq!(action["permission"], Value::Null, "{action}");
        }
    }
    assert_eq!(
        named(&actions, "member.role_changed"),
        &json!({
            "action": "member.role_changed",
            "description": "A member changed the role of another.",
            "logs": ["workspace"],
            "resource_types": ["workspace"],
            "permission": "members.read",
        })
    );
    assert_eq!(
        named(&actions, "user.registered"),
        &json!({
            "action": "user.registered",
            "description": "The user registered an account.",
            "logs": ["user"],
            "resource_types": ["user"],
            "permission": null,
        })
    );
    // A refusal is in both logs, and every reader of a workspace's log reads it (AUDIT-004).
    let denied = named(&actions, "access.denied");
    assert_eq!(denied["logs"], json!(["user", "workspace"]));
    assert_eq!(denied["permission"], Value::Null);
    assert!(
        denied["resource_types"]
            .as_array()
            .unwrap()
            .contains(&json!("workspace"))
    );
}

#[tokio::test]
async fn audit_012_each_name_in_the_catalog_is_one_a_log_s_filter_takes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let actions = catalog(&api).await;
    let mut resources = BTreeSet::new();
    for action in &actions {
        for resource in action["resource_types"].as_array().unwrap() {
            resources.insert(resource.as_str().unwrap().to_owned());
        }
    }
    let names: Vec<String> = actions
        .iter()
        .map(|action| action["action"].as_str().unwrap().to_owned())
        .collect();

    // A filter takes at most 50 values (AUDIT-009).
    for (parameter, values) in [
        ("action", names),
        ("resource_type", resources.into_iter().collect()),
    ] {
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
    let actions = catalog(&api).await;

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
        let listed =
            |field: &str, value: &str| declared[field].as_array().unwrap().contains(&json!(value));
        assert!(listed("logs", log), "{entry}");
        assert!(listed("resource_types", resource), "{entry}");
        if log == "workspace" {
            let expected = declared["permission"].as_str().unwrap_or("-");
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
    let actions = catalog(&api).await;
    let document = api.document();
    let schemas = document["components"]["schemas"].as_object().unwrap();
    let documented: BTreeSet<&str> = schemas
        .values()
        .filter_map(|schema| schema["x-audit-action"].as_str())
        .collect();
    let details = |entry: &Value| {
        let reference = entry["allOf"][1]["properties"]["details"]["$ref"]
            .as_str()
            .unwrap();
        schemas[reference.rsplit('/').next().unwrap()].clone()
    };
    let entry = |name: &str| {
        let found = schemas
            .values()
            .find(|schema| schema["x-audit-action"] == name);
        assert!(found.is_some(), "{name} isn't documented");
        found.unwrap()
    };

    let listed: BTreeSet<&str> = actions
        .iter()
        .map(|action| action["action"].as_str().unwrap())
        .collect();
    assert_eq!(documented, listed);
    for action in &actions {
        let name = action["action"].as_str().unwrap();
        let schema = entry(name);
        assert_eq!(
            schema["allOf"][0],
            json!({ "$ref": "#/components/schemas/LoggedEntry" }),
            "{name}"
        );
        let properties = &schema["allOf"][1]["properties"];
        assert_eq!(properties["action"]["enum"], json!([name]));
        assert_eq!(
            properties["resource_type"]["enum"],
            action["resource_types"]
        );
        assert!(
            schema["description"]
                .as_str()
                .unwrap()
                .contains(action["description"].as_str().unwrap()),
            "{name}"
        );
        assert_eq!(details(schema)["type"], "object", "{name}");
    }
    let role_changed = entry("member.role_changed");
    assert!(
        role_changed["description"]
            .as_str()
            .unwrap()
            .contains("shows it to a reader whose role holds `members.read`")
    );
    let changed = details(role_changed);
    assert_eq!(changed["required"], json!(["from", "to"]));
    assert_eq!(
        changed["properties"]["from"]["$ref"],
        "#/components/schemas/Role"
    );
    let denied = details(entry("access.denied"));
    assert!(denied["properties"].get("permission").is_some());
    assert_eq!(details(entry("user.renamed"))["properties"], Value::Null);
}
