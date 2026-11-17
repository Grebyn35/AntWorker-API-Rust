//! The action catalog: every action that the modules declare, as `openapi.json` documents it
//! (AUDIT-012), each by the schema of its entries, with the schema of their details (AUDIT-014).

use serde::Serialize;
use utoipa::{
    ToSchema,
    openapi::{
        AllOfBuilder, ObjectBuilder, OneOfBuilder, OpenApi, Ref, RefOr, Schema, Type,
        extensions::ExtensionsBuilder,
    },
};

use super::{LoggedEntry, action::Action};

/// The extension that marks an action's schema among the components, with the action's name.
const ACTION_EXTENSION: &str = "x-audit-action";

/// The component of the action catalog, which holds every action's schema (AUDIT-012).
const CATALOG: &str = "AuditEntry";

/// An action that the catalog holds: a module's `audit.rs` lists each of its own, behind this
/// trait, whatever the type of its details.
pub(crate) trait Catalogued: Sync {
    /// The action's name.
    fn name(&self) -> &'static str;

    /// The schema of an entry of the action (AUDIT-014).
    fn entry(&self) -> RefOr<Schema>;

    /// The schema of its details, under the name of its component, with those it refers to.
    fn details(&self) -> Vec<(String, RefOr<Schema>)>;
}

impl<D: Serialize + ToSchema> Catalogued for Action<D> {
    fn name(&self) -> &'static str {
        Action::name(self)
    }

    fn entry(&self) -> RefOr<Schema> {
        entry_schema(self)
    }

    fn details(&self) -> Vec<(String, RefOr<Schema>)> {
        let mut schemas = vec![(D::name().into_owned(), D::schema())];
        D::schemas(&mut schemas);
        schemas
    }
}

/// Every action that the modules declare, in the order of their names.
pub(crate) struct Catalog {
    actions: Vec<&'static dyn Catalogued>,
}

impl Catalog {
    /// The catalog of the actions in `lists`, each a module's.
    pub(crate) fn new(lists: &[&'static [&'static dyn Catalogued]]) -> Self {
        let mut actions: Vec<&'static dyn Catalogued> =
            lists.iter().flat_map(|list| list.iter().copied()).collect();
        actions.sort_by_key(|action| action.name());
        Self { actions }
    }

    /// Adds the catalog to `document`: a component for each action, the schema of an entry of
    /// that action, marked as the action's, with the schema of its details and those it refers
    /// to (AUDIT-014), and the component that holds them all, in the order of the actions' names
    /// (AUDIT-012).
    pub(crate) fn document(&self, document: &mut OpenApi) {
        let components = document.components.get_or_insert_default();
        let mut catalog = OneOfBuilder::new().description(Some(
            "An entry of an audit log, one schema for each action, which its `action` names \
             (AUDIT-012).",
        ));
        for action in &self.actions {
            for (name, schema) in action.details() {
                components.schemas.entry(name).or_insert(schema);
            }
            let component = component(action.name());
            catalog = catalog.item(Ref::from_schema_name(component.as_str()));
            components.schemas.insert(component, action.entry());
        }
        components
            .schemas
            .insert(CATALOG.to_owned(), RefOr::T(Schema::OneOf(catalog.build())));
    }
}

/// The name of the component that holds the schema of an entry of the action `name`:
/// `member.removed`'s is `MemberRemovedEntry`.
fn component(name: &str) -> String {
    platform::http::component_name(name, "Entry")
}

/// The schema of an entry of `action`: a `LoggedEntry` whose `action` is the action's name, whose
/// `resource_type` is one of those it declares, and whose `details` are of its details' type,
/// described by what the action means, the logs that record it, and who in a workspace's log
/// reads it.
fn entry_schema<D: ToSchema>(action: &Action<D>) -> RefOr<Schema> {
    let mut logs = Vec::new();
    if action.in_own_logs() {
        logs.push("a user's own log");
    }
    if action.in_workspace_logs() {
        logs.push("a workspace's log");
    }
    let readers = match action.permission() {
        Some(permission) => {
            format!(" A workspace's log shows it to a reader whose role holds `{permission}`.")
        }
        None if action.in_workspace_logs() => {
            " A workspace's log shows it to whoever reads the log.".to_owned()
        }
        None => String::new(),
    };
    let description = format!(
        "`{}`: {} It is recorded in {}.{readers}",
        action.name(),
        action.description(),
        logs.join(" and ")
    );
    let resource_types: Vec<&'static str> =
        action.resources().iter().map(|kind| kind.name()).collect();
    let specific = ObjectBuilder::new()
        .schema_type(Type::Object)
        .property(
            "action",
            ObjectBuilder::new()
                .schema_type(Type::String)
                .enum_values(Some([action.name()])),
        )
        .property(
            "resource_type",
            ObjectBuilder::new()
                .schema_type(Type::String)
                .enum_values(Some(resource_types)),
        )
        .property("details", Ref::from_schema_name(D::name()));
    AllOfBuilder::new()
        .item(Ref::from_schema_name(LoggedEntry::name()))
        .item(specific)
        .description(Some(description))
        .extensions(Some(
            ExtensionsBuilder::new()
                .add(ACTION_EXTENSION, action.name())
                .build(),
        ))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_014_an_action_s_component_is_named_after_it() {
        assert_eq!(component("member.removed"), "MemberRemovedEntry");
        assert_eq!(component("sign_in.failed"), "SignInFailedEntry");
        assert_eq!(
            component("verification_code.issued"),
            "VerificationCodeIssuedEntry"
        );
    }
}
