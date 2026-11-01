//! An audit entry, as a module records it.

use platform::types::UserId;

/// What happened, as `<resource>.<past-tense verb>`, such as `user.registered`. Each module
/// declares its own in its `audit.rs` (the audit spec), the one place clippy lets build one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Action(&'static str);

impl Action {
    /// The action `name`.
    pub(crate) const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The action's name.
    pub(crate) fn name(self) -> &'static str {
        self.0
    }
}

/// A kind of resource that entries happen to, such as `user` or `session`. Each module declares
/// its own in its `audit.rs` (the audit spec), the one place clippy lets build one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Resource(&'static str);

impl Resource {
    /// The kind of resource `name`.
    pub(crate) const fn new(name: &'static str) -> Self {
        Self(name)
    }

    /// The kind's name.
    pub(crate) fn name(self) -> &'static str {
        self.0
    }
}

/// What a module records about one change or security event (AUDIT-003). The log and the actor
/// come from the `AuditScope` it is recorded with, never from here (AUDIT-002).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    /// What happened.
    pub(crate) action: Action,
    /// The kind of resource it happened to.
    pub(crate) resource_type: Resource,
    /// The resource's ID.
    pub(crate) resource_id: Option<uuid::Uuid>,
    /// The user it happened to, when it isn't the actor.
    pub(crate) target: Option<UserId>,
    /// IDs and non-personal fields only (SEC-16).
    pub(crate) details: serde_json::Value,
}

impl Entry {
    /// An entry of `action` on the resource of the kind `resource_type` with the ID
    /// `resource_id`, with no target and no details.
    pub(crate) fn new(action: Action, resource_type: Resource, resource_id: uuid::Uuid) -> Self {
        Self {
            action,
            resource_type,
            resource_id: Some(resource_id),
            target: None,
            details: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}
