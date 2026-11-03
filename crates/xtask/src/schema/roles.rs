//! The rules of `check-schema` on the roles: what `owner` and `app` own, hold and have set
//! (DATA-12, DATA-19).

use serde_json::Value;

use super::rules::{MIGRATIONS_TABLE, Problem, list, strings, text};

/// The privileges `app` may hold on a table (DATA-19).
const DML: [&str; 4] = ["DELETE", "INSERT", "SELECT", "UPDATE"];

/// The tables on which `app` holds less than DML, with what it holds (DATA-19).
const APPEND_ONLY: [(&str, [&str; 2]); 1] = [("audit_entries", ["INSERT", "SELECT"])];

/// The role-level settings `app` has (DATA-19).
const APP_SETTINGS: [&str; 2] = ["statement_timeout", "idle_in_transaction_session_timeout"];

/// DATA-12 and DATA-19: what `owner` and `app` hold and own, and the privileges of functions.
pub fn check(catalog: &Value) -> Vec<Problem> {
    let mut problems = objects(catalog);
    problems.extend(roles(catalog));
    problems
}

/// DATA-12 and DATA-19: what `app` holds on each table, and who owns the schema's objects.
fn objects(catalog: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut problem = |subject: &str, message: String| problems.push((subject.to_owned(), message));
    for table in list(catalog, "tables") {
        let name = text(table, "name");
        let held = strings(table, "app");
        if let Some((_, only)) = APPEND_ONLY
            .iter()
            .find(|(append_only, _)| *append_only == name)
        {
            if held != only {
                problem(
                    name,
                    format!(
                        "app holds {} on {name}, not only INSERT and SELECT (DATA-19)",
                        held.join(", ")
                    ),
                );
            }
        } else if let Some(other) = held.iter().find(|privilege| !DML.contains(privilege)) {
            problem(
                name,
                format!(
                    "app holds {other} on {name}, beyond SELECT, INSERT, UPDATE and DELETE (DATA-19)"
                ),
            );
        }
        if table["owner"] != "owner" && name != MIGRATIONS_TABLE {
            problem(
                name,
                format!(
                    "{name} is owned by {}, not owner (DATA-12)",
                    text(table, "owner")
                ),
            );
        }
    }
    for sequence in list(catalog, "sequences") {
        let name = text(sequence, "name");
        if let Some(other) = strings(sequence, "app")
            .iter()
            .find(|privilege| **privilege != "USAGE")
        {
            problem(
                name,
                format!("app holds {other} on {name}, beyond USAGE (DATA-19)"),
            );
        }
    }
    for kind in ["types", "sequences", "functions"] {
        for object in list(catalog, kind) {
            let name = text(object, "name");
            if object["owner"] != "owner" {
                problem(
                    name,
                    format!(
                        "{name} is owned by {}, not owner (DATA-12)",
                        text(object, "owner")
                    ),
                );
            }
            if object["public_execute"] == true {
                problem(name, format!("PUBLIC may execute {name} (DATA-19)"));
            }
        }
    }
    problems
}

/// DATA-12 and DATA-19: the roles' privileges on the schema and on functions to come, what
/// `app` owns, and its settings.
fn roles(catalog: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut problem = |subject: &str, message: String| problems.push((subject.to_owned(), message));
    if catalog["default_function_execute"] != false {
        problem(
            "ALTER DEFAULT PRIVILEGES",
            "PUBLIC may execute the functions owner creates (DATA-19)".to_owned(),
        );
    }
    let settings = strings(catalog, "app_settings");
    for setting in APP_SETTINGS {
        if !settings
            .iter()
            .any(|set| set.starts_with(&format!("{setting}=")))
        {
            problem(
                setting,
                format!("app has no {setting} of its own (DATA-19)"),
            );
        }
    }
    if catalog["app_creates"] != false {
        problem(
            "CREATE",
            "app may create objects in the schema public (DATA-19)".to_owned(),
        );
    }
    if catalog["owner_creates"] != true {
        problem(
            "CREATE",
            "owner may not create objects in the schema public (DATA-12)".to_owned(),
        );
    }
    let schema_owner = text(catalog, "schema_owner");
    if schema_owner != "pg_database_owner" && schema_owner != text(catalog, "database_owner") {
        problem(
            "public",
            format!(
                "the schema public is owned by {schema_owner}, not the database's owner (DATA-12)"
            ),
        );
    }
    for owned in strings(catalog, "app_owns") {
        problem(
            owned,
            format!("app owns {owned}, and owns nothing (DATA-12)"),
        );
    }
    for elsewhere in strings(catalog, "app_elsewhere") {
        problem(
            elsewhere,
            format!("app holds privileges on {elsewhere}, outside the schema public (DATA-19)"),
        );
    }
    problems
}
