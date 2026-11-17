//! An audit entry, as a module records it, and as a list of a log shows it. An action, as a
//! module declares it, is in `action`, and the action catalog in `catalog`.

mod action;
mod catalog;

use platform::{
    authn::AuditScope,
    errors::Internal,
    types::{Invalid, Timestamp, UserId, WorkspaceId},
};
use serde::Serialize;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

// part: superadmins
pub(crate) use action::Access;
// end part: superadmins
pub(crate) use action::{Action, NoDetails, Resource};
pub(crate) use catalog::{Catalog, Catalogued};

/// The fewest days that `AUDIT_RETENTION_DAYS` takes: the floor of `deletable_audit_entries`,
/// through which retention deletes, and which shows no entry recorded more recently, as its
/// migration states (AUDIT-005).
pub(crate) const RETENTION_FLOOR_DAYS: u32 = 90;

/// How long each log keeps an entry (AUDIT-011).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Settings {
    /// Its days, from [`RETENTION_FLOOR_DAYS`] to 3,650.
    pub(crate) retention_days: u32,
}

/// What a module records about one change or security event (AUDIT-003), with `D`, the details
/// that its action declares, so an entry holds no other (AUDIT-014). The log and the actor come
/// from the `AuditScope` it is recorded with, never from here (AUDIT-002).
#[derive(Clone, Debug)]
pub(crate) struct Entry<D = NoDetails> {
    /// What happened.
    pub(crate) action: Action<D>,
    /// The kind of resource it happened to, one that the action declares (AUDIT-013).
    pub(crate) resource_type: Resource,
    /// The resource's ID.
    pub(crate) resource_id: Option<uuid::Uuid>,
    /// The user it happened to, if any, who may be its actor too, as a member who leaves a
    /// workspace is (WORKSPACES-014).
    pub(crate) target: Option<UserId>,
    /// IDs and non-personal fields only (SEC-18), of the type the action declares.
    pub(super) details: D,
}

impl<D: Default> Entry<D> {
    /// An entry of `action` on the resource of the kind `resource_type` with the ID
    /// `resource_id`, with no target, and the details that `D` holds by default, such as none.
    pub(crate) fn new(action: Action<D>, resource_type: Resource, resource_id: uuid::Uuid) -> Self {
        Self::detailed(action, resource_type, resource_id, D::default())
    }
}

impl<D> Entry<D> {
    /// An entry of `action` on the resource of the kind `resource_type` with the ID
    /// `resource_id`, with no target, and `details`.
    pub(crate) fn detailed(
        action: Action<D>,
        resource_type: Resource,
        resource_id: uuid::Uuid,
        details: D,
    ) -> Self {
        Self {
            action,
            resource_type,
            resource_id: Some(resource_id),
            target: None,
            details,
        }
    }

    /// The entry, with `target` as the user it happened to.
    pub(crate) fn targeting(self, target: Option<UserId>) -> Self {
        Self { target, ..self }
    }

    /// The entry, admitted to the log that `scope` names, when its action declares that kind of
    /// log and the entry's kind of resource; otherwise the internal error that keeps it out of
    /// every log (AUDIT-013).
    pub(super) fn admitted(&self, scope: AuditScope) -> Result<Admitted<'_, D>, Internal> {
        self.action.admits(scope.log(), self.resource_type)?;
        Ok(Admitted { entry: self, scope })
    }
}

/// An entry that its action admits to the log of `scope`, which only [`Entry::admitted`]
/// builds: what the statement that writes an entry takes, so no code writes one that its action
/// doesn't declare (AUDIT-013).
pub(super) struct Admitted<'a, D> {
    entry: &'a Entry<D>,
    scope: AuditScope,
}

impl<'a, D> Admitted<'a, D> {
    /// The entry.
    pub(super) fn entry(&self) -> &'a Entry<D> {
        self.entry
    }

    /// The scope that names its log and its actor.
    pub(super) fn scope(&self) -> AuditScope {
        self.scope
    }
}

/// An entry, as the export of the user whose log holds it, or who is the only member of the
/// workspace whose log holds it, holds it: its client only when it was the user's (the accounts
/// spec's "What an export holds", AUDIT-006).
#[derive(Debug, Serialize)]
pub(super) struct ExportedEntry {
    pub(super) id: uuid::Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) workspace_id: Option<WorkspaceId>,
    pub(super) action: String,
    pub(super) resource_type: String,
    pub(super) resource_id: Option<uuid::Uuid>,
    pub(super) actor_user_id: Option<UserId>,
    pub(super) target_user_id: Option<UserId>,
    pub(super) details: serde_json::Value,
    pub(super) created_at: Timestamp,
    pub(super) client_ip: Option<String>,
    pub(super) user_agent: Option<String>,
}

/// An entry, as a list of a log shows it (AUDIT-007, AUDIT-008).
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct LoggedEntry {
    /// The entry's ID.
    pub(super) id: uuid::Uuid,
    /// The workspace whose log holds it, or null for an entry of the reader's own log.
    pub(super) workspace_id: Option<WorkspaceId>,
    /// What happened, as `<resource>.<past-tense verb>`, such as `member.removed`.
    pub(super) action: String,
    /// The kind of resource it happened to, such as `workspace`.
    pub(super) resource_type: String,
    /// The resource's ID, or null for a resource that has none.
    pub(super) resource_id: Option<uuid::Uuid>,
    /// The user who did it, or null for an entry that no user's request made, such as a failed
    /// sign-in's, or once their account is deleted (AUDIT-006).
    pub(super) actor_user_id: Option<UserId>,
    /// The user it was done to, who may be its actor too, as a member who leaves a workspace is
    /// (WORKSPACES-014), or null for an entry done to no user, or once their account is deleted
    /// (AUDIT-006).
    pub(super) target_user_id: Option<UserId>,
    /// IDs and non-personal fields that the action records, such as a role (SEC-18), each action
    /// its own, which the action catalog documents (AUDIT-014).
    #[schema(value_type = std::collections::HashMap<String, serde_json::Value>)]
    pub(super) details: serde_json::Value,
    /// The trace ID of the request or the job that recorded it (API-11), or null.
    pub(super) trace_id: Option<String>,
    /// When it was recorded.
    pub(super) created_at: Timestamp,
    /// The client of the request that recorded it, to a reader who may see it, or null
    /// (AUDIT-010).
    pub(super) client: Option<EntryClient>,
}

/// The client of the request that recorded an entry: its IP address, and its user agent, which a
/// request may not send (AUDIT-003, AUDIT-010).
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct EntryClient {
    /// The client's IP address, as the trusted proxies gave it (SEC-13).
    pub(super) ip: String,
    /// The client's `User-Agent`, or null for a request that sent none.
    pub(super) user_agent: Option<String>,
}

/// What a list of a log keeps: the entries that match one value of each filter given, and that
/// were recorded at or after `from`, and before `to` (AUDIT-009).
#[derive(Clone, Debug, Default)]
pub(crate) struct EntryFilter {
    /// Only the entries of these actions.
    pub(crate) actions: Option<Vec<ActionName>>,
    /// Only the entries that happened to these kinds of resource.
    pub(crate) resource_types: Option<Vec<ResourceName>>,
    /// Only the entries that happened to these resources.
    pub(crate) resource_ids: Option<Vec<uuid::Uuid>>,
    /// Only the entries that these users did.
    pub(crate) actors: Option<Vec<UserId>>,
    /// Only the entries that were done to these users.
    pub(crate) targets: Option<Vec<UserId>>,
    /// Only the entries recorded at this moment or after.
    pub(crate) from: Option<Timestamp>,
    /// Only the entries recorded before this moment.
    pub(crate) to: Option<Timestamp>,
}

/// The most characters of an action's name, or a resource type's, that a filter takes (AUDIT-009).
const NAME_MAX_CHARS: usize = 100;

/// Whether `word` is a word of an action's name or a resource type's: lowercase ASCII letters,
/// digits and underscores, starting with a letter (AUDIT-009).
fn is_word(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_lowercase())
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// The name of an action that a filter keeps, such as `member.removed`: two words joined by a dot
/// (AUDIT-009).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionName(String);

impl ActionName {
    /// The action's name in `value`, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        let words = value.split_once('.');
        if value.len() <= NAME_MAX_CHARS
            && words.is_some_and(|(resource, verb)| is_word(resource) && is_word(verb))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err(Invalid::new(
                "invalid",
                "An action is two words of lowercase letters, digits and underscores joined by a \
                 dot, such as member.removed.",
            ))
        }
    }

    /// The name.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The name of a kind of resource that a filter keeps, such as `workspace`: one word
/// (AUDIT-009).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResourceName(String);

impl ResourceName {
    /// The kind's name in `value`, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        if value.len() <= NAME_MAX_CHARS && is_word(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(Invalid::new(
                "invalid",
                "A resource type is a word of lowercase letters, digits and underscores, such as \
                 workspace.",
            ))
        }
    }

    /// The name.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialSchema for ActionName {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .max_length(Some(NAME_MAX_CHARS))
            .pattern(Some("^[a-z][a-z0-9_]*\\.[a-z][a-z0-9_]*$"))
            .into()
    }
}

impl PartialSchema for ResourceName {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .max_length(Some(NAME_MAX_CHARS))
            .pattern(Some("^[a-z][a-z0-9_]*$"))
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test declares an action and resources of its own, as an audit.rs does"
    )]
    fn audit_013_an_entry_is_admitted_only_to_a_log_and_of_a_resource_its_action_declares() {
        const THING: Resource = Resource::new("thing");
        const OTHER: Resource = Resource::new("other");
        let id = uuid::Uuid::nil();
        let scope = AuditScope::attempt(UserId::from_uuid(id));
        let own: Action = Action::new("thing.done", "Done.", &[THING]).in_own_log();
        let shared: Action =
            Action::new("thing.shared", "Shared.", &[THING]).in_workspace_log_for_every_reader();

        let admitted = Entry::new(own, THING, id);
        let admitted = admitted.admitted(scope).unwrap();
        assert_eq!(admitted.entry().action.name(), "thing.done");
        assert_eq!(admitted.scope(), scope);
        assert!(Entry::new(own, OTHER, id).admitted(scope).is_err());
        assert!(Entry::new(shared, THING, id).admitted(scope).is_err());
    }

    #[test]
    fn audit_009_an_action_and_a_resource_type_are_words_of_lowercase_ascii() {
        for action in ["member.removed", "sign_in.failed", "a1.b_2"] {
            assert_eq!(ActionName::parse(action).unwrap().as_str(), action);
        }
        let long = format!("{}.b", "a".repeat(98));
        assert!(ActionName::parse(&long).is_ok());
        for refused in [
            "member",
            "member.",
            ".removed",
            "member.removed.again",
            "Member.removed",
            "member.re-moved",
            "1member.removed",
            "member._removed",
            "membér.removed",
            &format!("{}.bc", "a".repeat(98)),
            "",
        ] {
            assert_eq!(
                ActionName::parse(refused).unwrap_err().code,
                "invalid",
                "{refused}"
            );
        }
        assert_eq!(
            ResourceName::parse("verification_code").unwrap().as_str(),
            "verification_code"
        );
        assert!(ResourceName::parse(&"a".repeat(100)).is_ok());
        for refused in [
            "member.removed",
            "Workspace",
            "_code",
            "a b",
            &"a".repeat(101),
            "",
        ] {
            assert_eq!(
                ResourceName::parse(refused).unwrap_err().code,
                "invalid",
                "{refused}"
            );
        }
    }
}
