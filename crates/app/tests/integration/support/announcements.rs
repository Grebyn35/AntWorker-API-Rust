//! Routes that only the tests serve, which publish events of the tests' own, for the behaviors of
//! the hub and the stream that no route of the API shows on demand (TEST-05): events to the
//! caller, and events in a workspace that need a permission every role holds, or one that the
//! `member` role lacks. With them, the event the tests publish from units of work of their own.

use axum::{Extension, http::StatusCode};
use platform::{
    authn::Principal,
    authz::{Authorize, Authorizer, Permission},
    errors::{PlatformError, Problem},
    events::{UserEvent, WorkspaceEvent},
    http::{Json as Body, Routes},
    types::{Client, WorkspaceId},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::routes;

/// Reading the workspace, which every role holds, as `access`'s matrix names it.
struct Reading;

impl Permission for Reading {
    const NAME: &'static str = "workspace.read";
}

/// Removing members, which the `member` role lacks, as `access`'s matrix names it.
struct Removing;

impl Permission for Removing {
    const NAME: &'static str = "members.remove";
}

#[expect(
    clippy::disallowed_methods,
    reason = "a test-only route declares its events where it publishes them"
)]
pub mod published {
    use super::{Announced, Note, Posted, Reading, Removing, UserEvent, WorkspaceEvent};

    /// An announcement to its caller.
    pub(super) const TO_USER: UserEvent<Announced> = UserEvent::new("announcement.made");
    /// An announcement to every member of a workspace.
    pub(super) const TO_MEMBERS: WorkspaceEvent<Reading, Posted> =
        WorkspaceEvent::new("announcement.posted");
    /// An announcement to the members of a workspace who may remove members.
    pub(super) const TO_ADMINS: WorkspaceEvent<Removing, Posted> =
        WorkspaceEvent::new("announcement.restricted");
    /// A note that a test publishes to a user from a unit of work of its own.
    pub const NOTED: UserEvent<Note> = UserEvent::new("note.made");
}

/// The payload of an announcement to its caller.
#[derive(Serialize, ToSchema)]
pub struct Announced {
    /// Its number, from 1, in the unit of work that published it.
    n: u32,
}

/// The payload of an announcement in a workspace.
#[derive(Serialize, ToSchema)]
pub struct Posted {
    /// The workspace.
    workspace_id: WorkspaceId,
    /// Its number, from 1, in the unit of work that published it.
    n: u32,
}

/// The payload of a note, which a test tells apart by its label, if it gives one.
#[derive(Serialize, ToSchema)]
pub struct Note {
    /// What the test tells the note by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// What the announcing routes take.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct Announcement {
    /// How many events to publish, each with its number from 1 as `n`.
    count: u32,
    /// For a workspace's: whether only the members who may remove members receive them.
    #[serde(default)]
    restricted: bool,
}

/// Publishes `count` events to the caller, in one unit of work.
#[utoipa::path(
    post,
    path = "/v1/announcements",
    request_body = Announcement,
    responses((status = 204, description = "The events are published.")),
)]
async fn to_caller(
    Extension(authorizer): Extension<Authorizer>,
    client: Client,
    principal: Principal,
    Body(announcement): Body<Announcement>,
) -> Result<StatusCode, Problem<PlatformError>> {
    let mut uow = authorizer
        .database
        .begin(&client)
        .await
        .map_err(PlatformError::from)?;
    for n in 1..=announcement.count {
        uow.publish_to_user(&published::TO_USER, principal.user_id(), &Announced { n })
            .await
            .map_err(PlatformError::from)?;
    }
    uow.commit().await.map_err(PlatformError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Publishes `count` events to the workspace's members, or to those who may remove members, in
/// one unit of work.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{workspace_id}/announcements",
    params(("workspace_id" = WorkspaceId, Path, description = "The workspace.")),
    request_body = Announcement,
    responses((status = 204, description = "The events are published.")),
)]
async fn to_members(
    authorize: Authorize<Reading>,
    Body(announcement): Body<Announcement>,
) -> Result<StatusCode, Problem<PlatformError>> {
    let (mut uow, authorized) = authorize.open().await?;
    let workspace = authorized.workspace_id();
    for n in 1..=announcement.count {
        let payload = Posted {
            workspace_id: workspace,
            n,
        };
        if announcement.restricted {
            uow.publish_to_workspace(&published::TO_ADMINS, workspace, &payload)
                .await
        } else {
            uow.publish_to_workspace(&published::TO_MEMBERS, workspace, &payload)
                .await
        }
        .map_err(PlatformError::from)?;
    }
    uow.commit().await.map_err(PlatformError::from)?;
    Ok(StatusCode::NO_CONTENT)
}

/// The announcing routes, which the tests of the stream add to the API's.
pub fn routes() -> Routes {
    Routes::new()
        .events(&[
            &published::TO_USER,
            &published::TO_MEMBERS,
            &published::TO_ADMINS,
            &published::NOTED,
        ])
        .user(routes!(to_caller), &[])
        .merge(Routes::new().workspace::<Reading>(routes!(to_members), &[]))
}
