//! The rules `check-schema` checks a catalog against. Each problem names the object it concerns,
//! which the check finds in the migrations to give it a line.

use std::collections::BTreeSet;

use serde_json::Value;

/// A problem: the object it concerns, and what is wrong.
pub type Problem = (String, String);

/// The migrator's own table, outside the product schema (DATA-02).
const MIGRATIONS_TABLE: &str = "_sqlx_migrations";

/// The privileges `app` may hold on a table (DATA-19).
const DML: [&str; 4] = ["DELETE", "INSERT", "SELECT", "UPDATE"];

/// The tables on which `app` holds less than DML, with what it holds (DATA-19).
const APPEND_ONLY: [(&str, [&str; 2]); 1] = [("audit_entries", ["INSERT", "SELECT"])];

/// The role-level settings `app` has (DATA-19).
const APP_SETTINGS: [&str; 2] = ["statement_timeout", "idle_in_transaction_session_timeout"];

/// The soft-delete columns no table has (DATA-06).
const SOFT_DELETES: [&str; 3] = ["deleted_at", "is_deleted", "deleted"];

/// Every problem the catalog has. `bookkeeping` lists the tables of `platform`'s bookkeeping,
/// which architecture.md §5 gives `platform`.
pub fn check(catalog: &Value, bookkeeping: &[String]) -> Vec<Problem> {
    let mut problems = Vec::new();
    let tables: Vec<&Value> = list(catalog, "tables")
        .filter(|table| table["name"] != MIGRATIONS_TABLE)
        .collect();
    for table in &tables {
        problems.extend(columns(table));
        problems.extend(standard_columns(catalog, table, bookkeeping));
    }
    problems.extend(shared_trigger(&tables, bookkeeping));
    problems.extend(names(catalog));
    problems.extend(foreign_keys(catalog));
    problems.extend(grants(catalog));
    problems
}

/// The array at `key`, as values.
fn list<'a>(catalog: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    catalog[key].as_array().into_iter().flatten()
}

/// The strings of the array at `key` of `value`.
fn strings<'a>(value: &'a Value, key: &str) -> Vec<&'a str> {
    list(value, key).filter_map(Value::as_str).collect()
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// DATA-01 and DATA-06: a table's name, its timestamps, and no soft-delete column.
fn columns(table: &Value) -> Vec<Problem> {
    let name = text(table, "name");
    let mut problems = Vec::new();
    if !is_snake_case(name) || !name.ends_with('s') {
        problems.push((
            name.to_owned(),
            format!("the table {name} isn't a plural snake_case name (DATA-01)"),
        ));
    }
    for column in list(table, "columns") {
        let column_name = text(column, "name");
        if text(column, "type").starts_with("timestamp") && !column_name.ends_with("_at") {
            problems.push((
                column_name.to_owned(),
                format!("{name}.{column_name} is a timestamp, so its name is <event>_at (DATA-01)"),
            ));
        }
        if SOFT_DELETES.contains(&column_name) {
            problems.push((
                column_name.to_owned(),
                format!("{name}.{column_name} marks rows deleted: rows are deleted, not flagged (DATA-06)"),
            ));
        }
    }
    problems
}

/// The column `name` of `table`.
fn column<'a>(table: &'a Value, name: &str) -> Option<&'a Value> {
    list(table, "columns").find(|column| column["name"] == name)
}

/// DATA-02: `id uuid PRIMARY KEY DEFAULT uuidv7()`, or a composite key for a join table,
/// `created_at timestamptz NOT NULL DEFAULT now()`, and `updated_at` and `version` on a table
/// whose rows are edited, which is one that `app` may update and that isn't `platform`'s
/// bookkeeping. A bookkeeping table has neither.
fn standard_columns(catalog: &Value, table: &Value, bookkeeping: &[String]) -> Vec<Problem> {
    let name = text(table, "name");
    let mut problems = Vec::new();
    let mut problem =
        |message: String| problems.push((name.to_owned(), format!("{name}: {message} (DATA-02)")));
    let key: Vec<&str> = list(catalog, "constraints")
        .find(|constraint| constraint["table"] == name && constraint["type"] == "p")
        .map(|constraint| strings(constraint, "columns"))
        .unwrap_or_default();
    match column(table, "id") {
        Some(id) => {
            if id["type"] != "uuid" || id["default"] != "uuidv7()" || key != ["id"] {
                problem("its id isn't `id uuid PRIMARY KEY DEFAULT uuidv7()`".to_owned());
            }
        }
        None if key.len() < 2 => {
            problem("it has no id, and isn't a join table with a composite primary key".to_owned());
        }
        // A pure join table's composite key stands in for its id; a table whose rows are edited,
        // such as one with a role, isn't one.
        None if strings(table, "app").contains(&"UPDATE") => {
            problem(
                "it has no id, and its rows are edited, since app may update them, so it isn't a \
                 pure join table"
                    .to_owned(),
            );
        }
        None => {}
    }
    let created = column(table, "created_at");
    let standard = created.is_some_and(|created| {
        created["type"] == "timestamp with time zone"
            && created["not_null"] == true
            && created["default"] == "now()"
    });
    if !standard {
        problem("its created_at isn't `timestamptz NOT NULL DEFAULT now()`".to_owned());
    }
    let has_versions = column(table, "updated_at").is_some() && column(table, "version").is_some();
    if bookkeeping.iter().any(|table| table == name) {
        if column(table, "updated_at").is_some() || column(table, "version").is_some() {
            problem(
                "a table of platform's bookkeeping has neither updated_at nor version".to_owned(),
            );
        }
    } else if strings(table, "app").contains(&"UPDATE") {
        if !has_versions {
            problem(
                "its rows are edited, since app may update them, so it has updated_at and version"
                    .to_owned(),
            );
        }
        if !list(table, "triggers").any(|trigger| trigger["before_update_row"] == true) {
            problem("its rows are edited, so the shared trigger maintains updated_at and version before each update".to_owned());
        }
    }
    problems
}

/// DATA-02: one function maintains `updated_at` and `version` for every edited table.
fn shared_trigger(tables: &[&Value], bookkeeping: &[String]) -> Vec<Problem> {
    let functions: BTreeSet<&str> = tables
        .iter()
        .filter(|table| {
            !bookkeeping
                .iter()
                .any(|name| table["name"] == name.as_str())
        })
        .flat_map(|table| list(table, "triggers"))
        .filter(|trigger| trigger["before_update_row"] == true)
        .map(|trigger| text(trigger, "function"))
        .collect();
    if functions.len() > 1 {
        let listed: Vec<&str> = functions.into_iter().collect();
        vec![(
            listed.join(", "),
            format!(
                "updated_at and version are maintained by {}, not one shared function (DATA-02)",
                listed.join(", ")
            ),
        )]
    } else {
        Vec::new()
    }
}

/// DATA-01: constraints and indexes are named `<table>_<columns>_<kind>`, and a foreign key's
/// columns `<singular>_id`. A check may name some of the columns it reads.
fn names(catalog: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    for constraint in list(catalog, "constraints") {
        let (name, table) = (text(constraint, "name"), text(constraint, "table"));
        if table == MIGRATIONS_TABLE {
            continue;
        }
        let columns = strings(constraint, "columns");
        let expected = match text(constraint, "type") {
            "p" => vec![format!("{table}_pkey")],
            "u" => vec![format!("{table}_{}_key", columns.join("_"))],
            "f" => vec![format!("{table}_{}_fkey", columns.join("_"))],
            "c" => subsequences(&columns)
                .into_iter()
                .map(|named| format!("{table}_{named}_check"))
                .collect(),
            _ => continue,
        };
        if !expected.iter().any(|expected| expected == name) {
            problems.push((
                name.to_owned(),
                format!(
                    "the constraint {name} of {table} isn't named `{}` (DATA-01)",
                    expected.first().map_or("", String::as_str)
                ),
            ));
        }
        if constraint["type"] == "f" {
            for column in columns.iter().filter(|column| !column.ends_with("_id")) {
                problems.push((
                    name.to_owned(),
                    format!(
                        "{table}.{column} is a foreign key, so its name is <singular>_id (DATA-01)"
                    ),
                ));
            }
        }
    }
    for index in list(catalog, "indexes") {
        let (name, table) = (text(index, "name"), text(index, "table"));
        if index["constraint"] == true || table == MIGRATIONS_TABLE {
            continue;
        }
        let kind = if index["unique"] == true {
            "key"
        } else {
            "idx"
        };
        let columns: Vec<Option<&str>> = list(index, "columns").map(Value::as_str).collect();
        let fits = match columns.iter().copied().collect::<Option<Vec<&str>>>() {
            Some(columns) => name == format!("{table}_{}_{kind}", columns.join("_")),
            // An expression has no column to name.
            None => name.starts_with(&format!("{table}_")) && name.ends_with(&format!("_{kind}")),
        };
        if !fits {
            problems.push((
                name.to_owned(),
                format!(
                    "the index {name} of {table} isn't named `{table}_<columns>_{kind}` (DATA-01)"
                ),
            ));
        }
    }
    problems
}

/// Every non-empty subsequence of `columns`, joined with `_`.
fn subsequences(columns: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    let count = columns.len().min(8);
    for mask in 1_u32..(1 << count) {
        let picked: Vec<&str> = (0..count)
            .filter(|bit| mask & (1 << bit) != 0)
            .map(|bit| columns[bit])
            .collect();
        found.push(picked.join("_"));
    }
    found
}

/// DATA-05: each foreign key has an explicit `ON DELETE` of `CASCADE` or `RESTRICT`, and an
/// index whose leading columns are its own.
fn foreign_keys(catalog: &Value) -> Vec<Problem> {
    let mut problems = Vec::new();
    for constraint in list(catalog, "constraints").filter(|c| c["type"] == "f") {
        let (name, table) = (text(constraint, "name"), text(constraint, "table"));
        if !matches!(text(constraint, "on_delete"), "c" | "r") {
            problems.push((
                name.to_owned(),
                format!("the foreign key {name} has no ON DELETE CASCADE or RESTRICT (DATA-05)"),
            ));
        }
        let columns: BTreeSet<&str> = strings(constraint, "columns").into_iter().collect();
        let indexed = list(catalog, "indexes")
            .filter(|index| index["table"] == table && index["partial"] != true)
            .any(|index| {
                let leading: BTreeSet<&str> = list(index, "columns")
                    .take(columns.len())
                    .filter_map(Value::as_str)
                    .collect();
                leading == columns
            });
        if !indexed {
            problems.push((
                name.to_owned(),
                format!("the foreign key {name} has no index whose leading columns are its own (DATA-05)"),
            ));
        }
    }
    problems
}

/// DATA-12 and DATA-19: what `owner` and `app` hold and own, and the privileges of functions.
fn grants(catalog: &Value) -> Vec<Problem> {
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
    for kind in ["types", "functions"] {
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

fn is_snake_case(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.split('_').all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A catalog that follows every rule: an edited table, a join table, a bookkeeping table and
    /// an append-only table.
    fn catalog() -> Value {
        let id = json!({ "name": "id", "type": "uuid", "not_null": true, "default": "uuidv7()" });
        let created = json!({ "name": "created_at", "type": "timestamp with time zone", "not_null": true, "default": "now()" });
        let updated = json!({ "name": "updated_at", "type": "timestamp with time zone", "not_null": true, "default": "now()" });
        let version =
            json!({ "name": "version", "type": "bigint", "not_null": true, "default": "1" });
        let user_id =
            json!({ "name": "user_id", "type": "uuid", "not_null": true, "default": null });
        let trigger = json!([{ "function": "set_row_version", "before_update_row": true }]);
        let dml = json!(["DELETE", "INSERT", "SELECT", "UPDATE"]);
        json!({
            "tables": [
                { "name": "users", "owner": "owner", "app": dml, "columns": [id, created, updated, version], "triggers": trigger },
                { "name": "user_roles", "owner": "owner", "app": ["DELETE", "INSERT", "SELECT"], "columns": [user_id, created] , "triggers": [] },
                { "name": "rate_limit_buckets", "owner": "owner", "app": dml, "columns": [id, created], "triggers": [] },
                { "name": "audit_entries", "owner": "owner", "app": ["INSERT", "SELECT"], "columns": [id, created, user_id], "triggers": [] },
                { "name": "_sqlx_migrations", "owner": "owner", "app": ["SELECT"], "columns": [], "triggers": [] },
            ],
            "constraints": [
                { "name": "users_pkey", "table": "users", "type": "p", "columns": ["id"], "on_delete": " " },
                { "name": "user_roles_pkey", "table": "user_roles", "type": "p", "columns": ["user_id", "role"], "on_delete": " " },
                { "name": "rate_limit_buckets_pkey", "table": "rate_limit_buckets", "type": "p", "columns": ["id"], "on_delete": " " },
                { "name": "audit_entries_pkey", "table": "audit_entries", "type": "p", "columns": ["id"], "on_delete": " " },
                { "name": "audit_entries_user_id_fkey", "table": "audit_entries", "type": "f", "columns": ["user_id"], "on_delete": "c" },
                { "name": "audit_entries_scope_check", "table": "audit_entries", "type": "c", "columns": ["scope", "user_id"], "on_delete": " " },
                { "name": "users_email_not_null", "table": "users", "type": "n", "columns": ["email"], "on_delete": " " },
            ],
            "indexes": [
                { "name": "users_pkey", "table": "users", "unique": true, "partial": false, "constraint": true, "columns": ["id"] },
                { "name": "audit_entries_user_id_idx", "table": "audit_entries", "unique": false, "partial": false, "constraint": false, "columns": ["user_id"] },
                { "name": "users_lower_email_key", "table": "users", "unique": true, "partial": false, "constraint": false, "columns": [null] },
            ],
            "functions": [{ "name": "set_row_version", "owner": "owner", "public_execute": false }],
            "types": [{ "name": "audit_scope", "owner": "owner" }],
            "default_function_execute": false,
            "app_settings": ["statement_timeout=30s", "idle_in_transaction_session_timeout=30s"],
            "app_creates": false,
            "owner_creates": true,
            "schema_owner": "pg_database_owner",
            "database_owner": "postgres",
            "app_owns": [],
            "app_elsewhere": [],
        })
    }

    fn messages(catalog: &Value) -> Vec<String> {
        check(catalog, &["rate_limit_buckets".to_owned()])
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }

    #[test]
    fn data_02_a_schema_that_follows_the_rules_passes() {
        assert_eq!(messages(&catalog()), Vec::<String>::new());
    }

    #[test]
    fn data_01_tables_columns_constraints_and_indexes_are_named_by_the_rule() {
        let mut catalog = catalog();
        catalog["tables"][1]["name"] = json!("UserRole");
        catalog["tables"][0]["columns"][1]["name"] = json!("created");
        catalog["constraints"][4]["name"] = json!("fk_audit_user");
        catalog["constraints"][5]["name"] = json!("audit_entries_check");
        catalog["indexes"][1]["name"] = json!("audit_entries_user_idx");
        let found = messages(&catalog);
        for expected in [
            "the table UserRole isn't a plural snake_case name (DATA-01)",
            "users.created is a timestamp, so its name is <event>_at (DATA-01)",
            "the constraint fk_audit_user of audit_entries isn't named `audit_entries_user_id_fkey` (DATA-01)",
            "the constraint audit_entries_check of audit_entries isn't named `audit_entries_scope_check` (DATA-01)",
            "the index audit_entries_user_idx of audit_entries isn't named `audit_entries_<columns>_idx` (DATA-01)",
        ] {
            assert!(
                found.iter().any(|m| m == expected),
                "{expected} in {found:#?}"
            );
        }
    }

    #[test]
    fn data_02_a_join_table_whose_rows_are_edited_has_an_id() {
        let mut catalog = catalog();
        catalog["tables"][1]["app"] = json!(["DELETE", "INSERT", "SELECT", "UPDATE"]);
        let found = messages(&catalog);
        let expected = "user_roles: it has no id, and its rows are edited, since app may update \
                        them, so it isn't a pure join table (DATA-02)";
        assert!(found.iter().any(|m| m == expected), "{found:#?}");
    }

    #[test]
    fn data_02_edited_tables_have_versions_and_bookkeeping_and_append_only_ones_have_none() {
        let mut catalog = catalog();
        catalog["tables"][0]["columns"] = json!([
            catalog["tables"][0]["columns"][0],
            catalog["tables"][0]["columns"][1]
        ]);
        catalog["tables"][0]["triggers"] = json!([]);
        catalog["tables"][2]["columns"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "name": "version", "type": "bigint", "not_null": true, "default": "1" }));
        catalog["tables"][3]["columns"][0]["default"] = json!("gen_random_uuid()");
        assert_eq!(
            messages(&catalog),
            [
                "users: its rows are edited, since app may update them, so it has updated_at and version (DATA-02)",
                "users: its rows are edited, so the shared trigger maintains updated_at and version before each update (DATA-02)",
                "rate_limit_buckets: a table of platform's bookkeeping has neither updated_at nor version (DATA-02)",
                "audit_entries: its id isn't `id uuid PRIMARY KEY DEFAULT uuidv7()` (DATA-02)",
            ]
        );
    }

    #[test]
    fn data_05_foreign_keys_delete_explicitly_and_are_indexed() {
        let mut catalog = catalog();
        catalog["constraints"][4]["on_delete"] = json!("a");
        catalog["indexes"][1]["partial"] = json!(true);
        assert_eq!(
            messages(&catalog),
            [
                "the foreign key audit_entries_user_id_fkey has no ON DELETE CASCADE or RESTRICT (DATA-05)",
                "the foreign key audit_entries_user_id_fkey has no index whose leading columns are its own (DATA-05)",
            ]
        );
    }

    #[test]
    fn data_06_no_table_has_a_soft_delete_column() {
        let mut catalog = catalog();
        catalog["tables"][0]["columns"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "name": "is_deleted", "type": "boolean", "not_null": true, "default": "false" }));
        assert_eq!(
            messages(&catalog),
            ["users.is_deleted marks rows deleted: rows are deleted, not flagged (DATA-06)"]
        );
    }

    #[test]
    fn data_19_app_holds_only_dml_and_only_insert_and_select_on_audit_entries() {
        let mut catalog = catalog();
        catalog["tables"][0]["app"] = json!(["DELETE", "INSERT", "SELECT", "TRUNCATE", "UPDATE"]);
        catalog["tables"][3]["app"] = json!(["DELETE", "INSERT", "SELECT"]);
        catalog["functions"][0]["public_execute"] = json!(true);
        catalog["default_function_execute"] = json!(true);
        catalog["app_settings"] = json!(["statement_timeout=30s"]);
        catalog["app_creates"] = json!(true);
        assert_eq!(
            messages(&catalog),
            [
                "app holds TRUNCATE on users, beyond SELECT, INSERT, UPDATE and DELETE (DATA-19)",
                "app holds DELETE, INSERT, SELECT on audit_entries, not only INSERT and SELECT (DATA-19)",
                "PUBLIC may execute set_row_version (DATA-19)",
                "PUBLIC may execute the functions owner creates (DATA-19)",
                "app has no idle_in_transaction_session_timeout of its own (DATA-19)",
                "app may create objects in the schema public (DATA-19)",
            ]
        );
    }

    #[test]
    fn data_12_owner_owns_the_schema_s_objects_and_app_owns_nothing() {
        let mut catalog = catalog();
        catalog["types"][0]["owner"] = json!("postgres");
        catalog["owner_creates"] = json!(false);
        catalog["schema_owner"] = json!("owner");
        catalog["app_owns"] = json!(["notes"]);
        assert_eq!(
            messages(&catalog),
            [
                "audit_scope is owned by postgres, not owner (DATA-12)",
                "owner may not create objects in the schema public (DATA-12)",
                "the schema public is owned by owner, not the database's owner (DATA-12)",
                "app owns notes, and owns nothing (DATA-12)",
            ]
        );
    }
}
