//! The events `workspaces` publishes (API-19, the platform spec's events).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its events here (the platform spec's events)"
)]

use platform::events::{UserEvent, WorkspaceEvent};

use crate::modules::access::MembersRead;

/// The user joined a workspace, sent to their streams (WORKSPACES-001).
pub(super) const MEMBERSHIP_CREATED: UserEvent = UserEvent::new("membership.created");
/// A member was removed, sent to the members who may list members (WORKSPACES-005).
pub(super) const MEMBER_REMOVED: WorkspaceEvent<MembersRead> =
    WorkspaceEvent::new("member.removed");
