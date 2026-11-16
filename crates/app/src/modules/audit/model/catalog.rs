//! The action catalog: every action that the modules declare, as `GET /v1/audit-catalog` lists it
//! (AUDIT-012), and as `openapi.json` documents it, each with the schema of its entries' details
//! (AUDIT-014).

use serde::Serialize;
use utoipa::{
    ToSchema,
    openapi::{
        AllOfBuilder, ObjectBuilder, OpenApi, Ref, RefOr, Schema, Type,
        extensions::ExtensionsBuilder,
    },
};

use super::{ActionName, LoggedEntry, ResourceName, action::Action};

/// The extension that marks an action's schema among the components, with the action's name.
const ACTION_EXTENSION: &str = "x-audit-action";

/// An action that the catalog lists: a module's `audit.rs` lists each of its own, behind this
/// trait, whatever the type of its details.
pub(crate) trait Catalogued: Sync {
    /// The action's name.
    fn name(&self) -> &'static str;

    /// The action, as `GET /v1/audit-catalog` lists it.
    fn listed(&self) -> AuditAction;

    /// The name of the component that holds its details' schema, and that schema with those it
    /// refers to.
    fn details(&self) -> (String, Vec<(String, RefOr<Schema>)>);
}

impl<D: Serialize + ToSchema> Catalogued for Action<D> {
    fn name(&self) -> &'static str {
        Action::name(self)
    }

    fn listed(&self) -> AuditAction {
        let mut logs = Vec::new();
        if self.in_own_logs() {
            logs.push(AuditLog::User);
        }
        if self.in_workspace_logs() {
            logs.push(AuditLog::Workspace);
        }
        AuditAction {
            action: self.name(),
            description: self.description(),
            logs,
            resource_types: self.resources().iter().map(|kind| kind.name()).collect(),
            permission: self.permission(),
        }
    }

    fn details(&self) -> (String, Vec<(String, RefOr<Schema>)>) {
        let mut schemas = vec![(D::name().into_owned(), D::schema())];
        D::schemas(&mut schemas);
        (D::name().into_owned(), schemas)
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

    /// What `GET /v1/audit-catalog` answers (AUDIT-012).
    pub(crate) fn listing(&self) -> AuditCatalog {
        AuditCatalog {
            actions: self.actions.iter().map(|action| action.listed()).collect(),
        }
    }

    /// Adds to `document` a component for each action, the schema of an entry of that action,
    /// marked as the action's, with the schema of its details and those it refers to (AUDIT-014).
    pub(crate) fn document(&self, document: &mut OpenApi) {
        let components = document.components.get_or_insert_default();
        for action in &self.actions {
            let listed = action.listed();
            let (details, schemas) = action.details();
            for (name, schema) in schemas {
                components.schemas.entry(name).or_insert(schema);
            }
            components
                .schemas
                .insert(component(listed.action), entry_schema(&listed, details));
        }
    }
}

/// The action catalog: every action that the API records (AUDIT-012).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(crate) struct AuditCatalog {
    /// Each action, in the order of their names.
    actions: Vec<AuditAction>,
}

/// An action, as the catalog lists it (AUDIT-012).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(crate) struct AuditAction {
    /// Its name, which an entry's `action` holds, such as `member.removed`.
    #[schema(value_type = ActionName)]
    action: &'static str,
    /// A sentence that tells what it means, which a client may show.
    description: &'static str,
    /// The logs that record it: `user`, a user's own, and `workspace`, a workspace's.
    logs: Vec<AuditLog>,
    /// The kinds of resource that its entries happen to, which an entry's `resource_type` holds.
    #[schema(value_type = Vec<ResourceName>)]
    resource_types: Vec<&'static str>,
    /// The permission that a reader of a workspace's log needs, beside `audit.read`, to read its
    /// entries there, or null for one that whoever reads the log reads, or that no workspace's
    /// log records (AUDIT-007).
    permission: Option<&'static str>,
}

/// A kind of log (AUDIT-012).
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AuditLog {
    /// A user's own log.
    User,
    /// A workspace's log.
    Workspace,
}

/// The name of the component that holds the schema of an entry of the action `name`:
/// `member.removed`'s is `MemberRemovedEntry`.
fn component(name: &str) -> String {
    platform::http::component_name(name, "Entry")
}

/// The schema of an entry of `action`, whose details' schema is the component `details`: a
/// `LoggedEntry` whose `action` is the action's name, whose `resource_type` is one of those it
/// declares, and whose `details` are of its details' type.
fn entry_schema(action: &AuditAction, details: String) -> RefOr<Schema> {
    let mut logs = Vec::new();
    if action.logs.iter().any(|log| matches!(log, AuditLog::User)) {
        logs.push("a user's own log");
    }
    if action
        .logs
        .iter()
        .any(|log| matches!(log, AuditLog::Workspace))
    {
        logs.push("a workspace's log");
    }
    let readers = match action.permission {
        Some(permission) => {
            format!(" A workspace's log shows it to a reader whose role holds `{permission}`.")
        }
        None if action
            .logs
            .iter()
            .any(|log| matches!(log, AuditLog::Workspace)) =>
        {
            " A workspace's log shows it to whoever reads the log.".to_owned()
        }
        None => String::new(),
    };
    let description = format!(
        "`{}`: {} It is recorded in {}.{readers}",
        action.action,
        action.description,
        logs.join(" and ")
    );
    let specific = ObjectBuilder::new()
        .schema_type(Type::Object)
        .property(
            "action",
            ObjectBuilder::new()
                .schema_type(Type::String)
                .enum_values(Some([action.action])),
        )
        .property(
            "resource_type",
            ObjectBuilder::new()
                .schema_type(Type::String)
                .enum_values(Some(action.resource_types.clone())),
        )
        .property("details", Ref::from_schema_name(details));
    AllOfBuilder::new()
        .item(Ref::from_schema_name(LoggedEntry::name()))
        .item(specific)
        .description(Some(description))
        .extensions(Some(
            ExtensionsBuilder::new()
                .add(ACTION_EXTENSION, action.action)
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
