//! An audit entry, as a module records it.

use platform::types::UserId;

/// What a module records about one change or security event (AUDIT-003). The log and the actor
/// come from the `AuditScope` it is recorded with, never from here (AUDIT-002).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    /// What happened, as `<resource>.<past-tense verb>`, such as `user.registered`.
    pub(crate) action: &'static str,
    /// The kind of resource it happened to, such as `user` or `session`.
    pub(crate) resource_type: &'static str,
    /// The resource's ID.
    pub(crate) resource_id: Option<uuid::Uuid>,
    /// The user it happened to, when it isn't the actor.
    pub(crate) target: Option<UserId>,
    /// IDs and non-personal fields only (SEC-16).
    pub(crate) details: serde_json::Value,
}

impl Entry {
    /// An entry of `action` on the resource `resource_type` with the ID `resource_id`, with no
    /// target and no details.
    pub(crate) fn new(
        action: &'static str,
        resource_type: &'static str,
        resource_id: uuid::Uuid,
    ) -> Self {
        Self {
            action,
            resource_type,
            resource_id: Some(resource_id),
            target: None,
            details: serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}
