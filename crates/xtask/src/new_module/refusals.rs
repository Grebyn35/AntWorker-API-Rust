//! What `new-module` refuses before it writes anything: a module the repository has or had, a
//! table or a permission of its name, and a resource whose type the module's code imports from
//! elsewhere, each of which would fail later, in the migration or the build, with a message that
//! says less.

use super::{MIGRATIONS, TEMPLATES, names::Names};
use crate::{
    migrations, openapi,
    repo::{File, Repo},
};

/// The permission matrix, whose permissions a module's are named after it (ACCESS-002).
const PERMISSIONS: &str = "crates/app/src/modules/access/permissions.rs";

/// Fails when the repository already has a module, a spec, a test file, a builder or a migration
/// of `names`' module, a table of its name, or a permission of its name, whose types the module's
/// would repeat.
pub(super) fn refuse_taken(repo: &Repo, names: &Names) -> Result<(), String> {
    let module = &names.module;
    let taken = [
        format!("crates/app/src/modules/{module}/"),
        format!("docs/spec/{module}.md"),
        format!("crates/app/tests/integration/{module}.rs"),
        format!("crates/app/tests/integration/support/{module}.rs"),
    ];
    // A migration is the module's when no module of a longer name also starts its description,
    // as check-table-ownership reads it.
    let longer: Vec<String> = repo
        .paths()
        .filter_map(|path| path.strip_prefix("crates/app/src/modules/"))
        .filter_map(|rest| rest.split_once('/').map(|(name, _)| name.to_owned()))
        .filter(|name| name.len() > module.len())
        .collect();
    let found = repo.paths().find(|path| {
        taken.iter().any(|taken| path.starts_with(taken.as_str()))
            || (migrations::of_module(path, module)
                && !longer.iter().any(|name| migrations::of_module(path, name)))
    });
    if let Some(path) = found {
        return Err(format!(
            "the repository has `{path}` already, so it has a module `{module}`, or had one"
        ));
    }
    let table = format!("CREATE TABLE {module} (");
    if let Some(file) = repo
        .files()
        .find(|file| file.path.starts_with(MIGRATIONS) && file.text.contains(&table))
    {
        return Err(format!(
            "{} creates a table `{module}` already, which the module's table would repeat",
            file.path
        ));
    }
    let permission = format!("\"{module}.");
    if repo
        .file(PERMISSIONS)
        .is_some_and(|file| file.text.contains(&permission))
    {
        return Err(format!(
            "{PERMISSIONS} holds a permission `{module}.` already, which the module's would repeat"
        ));
    }
    Ok(())
}

/// The types of Rust's prelude that the templates' code names, which a resource's type would
/// shadow.
const PRELUDE: [&str; 9] = [
    "Option", "Some", "None", "Result", "Ok", "Err", "String", "Vec", "Box",
];

/// Fails when the type of `names`' resource, such as `Page` for `page`, is one of Rust's prelude,
/// one that a template declares for itself, such as `Current`, or one that a template that names
/// the resource's type imports from elsewhere, such as `platform::lists::Page`: the module's files
/// would name two types by it.
pub(super) fn refuse_clashing(repo: &Repo, names: &Names) -> Result<(), String> {
    let resource = names.resource_type();
    let template = Names::template().resource_type();
    let prefix = format!("{TEMPLATES}/files/");
    let templates: Vec<&str> = repo
        .files()
        .filter(|file| file.path.starts_with(&prefix) && file.path.ends_with(".rs.tmpl"))
        .map(|file| file.text.as_str())
        .collect();
    let clashes = PRELUDE.contains(&resource.as_str())
        || templates.iter().any(|text| {
            declared(text).iter().any(|name| *name == resource && !name.contains(&template))
                // A name clashes only in a file that both imports it and names the resource's type.
                || (imported(text).contains(&resource) && names_type(text, &template))
        });
    if clashes {
        return Err(format!(
            "the resource `{}` would be the type `{resource}`, which a module's code names already: \
             name another resource, such as `just new-module {} <resource>`",
            names.resource, names.module
        ));
    }
    Ok(())
}

/// Fails when a type of `files`, the module's files, would take a schema name that a type of the
/// code takes already, such as `Role` for the resource `role`: the document keeps one schema of
/// each name (API-17).
pub(super) fn refuse_schema_names(repo: &Repo, files: &[(String, String)]) -> Result<(), String> {
    let taken = openapi::names::taken(repo);
    for (path, text) in files.iter().filter(|(path, _)| path.ends_with(".rs")) {
        let file = File {
            path: path.clone(),
            text: text.clone(),
        };
        if let Some((_, schema, _)) = openapi::names::schemas_of(&file)
            .into_iter()
            .find(|(_, schema, _)| taken.contains(schema))
        {
            return Err(format!(
                "{path} would declare a type whose schema name is `{schema}`, which a type of the \
                 code takes already, and the document keeps one schema of each name: name another \
                 resource (API-17)"
            ));
        }
    }
    Ok(())
}

/// The names of the types that `text`, a template, declares: its structs, enums, types and traits.
fn declared(text: &str) -> Vec<&str> {
    let words: Vec<&str> = text
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|word| !word.is_empty())
        .collect();
    words
        .windows(2)
        .filter(|pair| matches!(pair[0], "struct" | "enum" | "type" | "trait"))
        .map(|pair| pair[1])
        .filter(|name| name.starts_with(|c: char| c.is_ascii_uppercase()))
        .collect()
}

/// Whether `text` names the type `name` itself, rather than only names that hold it, such as
/// `SampleItemsError` or `NewSampleItem`.
fn names_type(text: &str, name: &str) -> bool {
    text.match_indices(name).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The names in `PascalCase` that the `use` declarations of `text`, a template, import.
fn imported(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for declaration in text.split(';') {
        let Some(start) = declaration
            .rfind("\nuse ")
            .map(|at| at + 1)
            .or_else(|| declaration.starts_with("use ").then_some(0))
        else {
            continue;
        };
        names.extend(
            declaration[start..]
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .filter(|word| word.starts_with(|c: char| c.is_ascii_uppercase()))
                .map(str::to_owned),
        );
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_03_a_module_that_exists_or_existed_is_refused() {
        let names = Names::new("order_items", None).unwrap();
        let spec = Repo::of(&[("docs/spec/order_items.md", "")]);
        let migration = Repo::of(&[("migrations/20260101000000_order_items_order_items.sql", "")]);
        let other = Repo::of(&[("migrations/20260101000000_orders_orders.sql", "")]);

        assert!(refuse_taken(&spec, &names).is_err());
        assert!(refuse_taken(&migration, &names).is_err());
        assert!(refuse_taken(&other, &names).is_ok());
        // `entries` is no module of `audit_entries_workspace_key`, whose module is `audit`.
        let entries = Names::new("entries", Some("entry")).unwrap();
        let audit = Repo::of(&[(
            "migrations/20261101130200_audit_entries_workspace_key.sql",
            "ALTER TABLE audit_entries ADD x int;",
        )]);
        assert!(refuse_taken(&audit, &entries).is_ok());
        let table = Repo::of(&[(
            "migrations/20261101120200_accounts_users.sql",
            "CREATE TABLE order_items (id uuid);",
        )]);
        assert!(refuse_taken(&table, &names).unwrap_err().contains("table"));
        let permission = Repo::of(&[(PERMISSIONS, "(\"order_items.read\", &[]),")]);
        assert!(
            refuse_taken(&permission, &names)
                .unwrap_err()
                .contains("permission")
        );
        // `orders_archive`, a longer module, owns `orders_archive_rows`, so `orders` may be written.
        let orders = Names::new("orders", None).unwrap();
        let longer = Repo::of(&[
            ("crates/app/src/modules/orders_archive/mod.rs", ""),
            ("migrations/20260101000000_orders_archive_rows.sql", ""),
        ]);
        assert!(refuse_taken(&longer, &orders).is_ok());
    }

    #[test]
    fn arch_03_a_resource_whose_type_the_templates_import_is_refused() {
        let repo = Repo::of(&[
            (
                "crates/xtask/templates/new-module/files/x/service.rs.tmpl",
                "//! A file.\n\nuse platform::{\n    lists::{List, Page},\n};\nuse super::model::SampleItem;\n",
            ),
            // The error type never names the resource's, so its `Category` clashes with nothing.
            (
                "crates/xtask/templates/new-module/files/x/error.rs.tmpl",
                "use platform::errors::{Category, ErrorCode};\n\npub enum SampleItemsError {}\n",
            ),
            (
                "crates/xtask/templates/new-module/files/x/model.rs.tmpl",
                "pub(crate) struct SampleItem {}\npub(crate) struct Current {}\n",
            ),
        ]);

        let refused = |module, resource| {
            refuse_clashing(&repo, &Names::new(module, Some(resource)).unwrap()).is_err()
        };

        assert!(refused("pages", "page"));
        assert!(!refused("order_items", "order_item"));
        assert!(!refused("categories", "category"));
        // A type of Rust's prelude, and one that a template declares, clash wherever they are.
        assert!(refused("results", "result"));
        assert!(refused("currents", "current"));
    }

    #[test]
    fn api_17_a_resource_whose_schema_name_the_code_takes_is_refused() {
        let repo = Repo::of(&[(
            "crates/app/src/modules/access/model.rs",
            "#[derive(Serialize, ToSchema)]\npub(crate) enum Role {}\n",
        )]);
        let model = |name: &str| {
            vec![(
                "crates/app/src/modules/x/model.rs".to_owned(),
                format!("#[derive(Serialize, ToSchema)]\npub(crate) struct {name} {{}}\n"),
            )]
        };

        let role = refuse_schema_names(&repo, &model("Role"));
        let order = refuse_schema_names(&repo, &model("OrderItem"));

        assert!(role.unwrap_err().contains("`Role`"));
        assert!(order.is_ok());
    }
}
