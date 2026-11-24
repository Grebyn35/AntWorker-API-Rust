//! What a workspace's members do with its invitations: listing them, reading one, changing its
//! role, sending it again and cancelling it (the invitations spec's Routes).

use axum::{extract::State, http::StatusCode};
use platform::{
    authz::Authorize,
    errors::{Problem, RequestError},
    http::{FieldErrors, IfMatch, MergePatch, PathIds, Query, ResourceResponse, Routes},
    lists::{Cursor, Limit, List, Page, Search},
    rate_limit::RateLimits,
    types::WorkspaceId,
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::routes;

use crate::modules::{
    access::{InvitationsCancel, InvitationsCreate, InvitationsRead, InvitationsUpdate, Role},
    invitations::{
        error::InvitationsError,
        model::{Invitation, NewRole},
        service::{self, Invitations},
    },
};

/// The routes of a workspace's invitations but inviting, which take the module's state.
pub(super) fn routes() -> Routes<Invitations> {
    Routes::new()
        .workspace::<InvitationsRead>(routes!(workspace_invitations), &[])
        .workspace::<InvitationsRead>(
            routes!(workspace_invitation),
            &[&InvitationsError::NotFound],
        )
        .workspace::<InvitationsUpdate>(
            routes!(change_invitation_role),
            &[&InvitationsError::NotFound],
        )
        .workspace::<InvitationsCreate>(routes!(resend_invitation), &[&InvitationsError::NotFound])
        .workspace::<InvitationsCancel>(routes!(cancel_invitation), &[&InvitationsError::NotFound])
}

/// The query of `GET /v1/workspaces/{workspace_id}/invitations` (INVITATIONS-012).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvitationsQuery {
    /// How many invitations a page holds.
    #[param(value_type = Option<Limit>, inline)]
    limit: Option<String>,
    /// The `next_cursor` of the page before.
    #[param(value_type = Option<Cursor>, inline)]
    cursor: Option<String>,
    /// Only the invitations whose email contains this, ignoring case (API-20).
    #[param(value_type = Option<Search>, inline)]
    search: Option<String>,
}

/// Lists the workspace's invitations, those that expired among them until the hourly sweep
/// deletes them, the latest created first.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/invitations",
    tag = "invitations",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        InvitationsQuery,
    ),
    responses(
        (status = 200, description = "A page of the workspace's invitations.", body = List<Invitation>),
    )
)]
async fn workspace_invitations(
    authorize: Authorize<InvitationsRead>,
    query: Query<InvitationsQuery>,
) -> Result<axum::Json<List<Invitation>>, Problem<InvitationsError>> {
    let params = query.params();
    let mut errors = FieldErrors::new();
    let page = Page::read(
        query.route(),
        params.limit.as_deref(),
        params.cursor.as_deref(),
        &mut errors,
    );
    let search = Search::read(params.search.as_deref(), &mut errors);
    let (page, search) = errors
        .finish_query(page.zip(search))
        .map_err(InvitationsError::from)?;
    let (mut uow, authorized) = authorize.open().await.map_err(InvitationsError::from)?;
    let list = service::invitations(&mut uow, &authorized, search.as_ref(), &page).await?;
    Ok(axum::Json(list))
}

/// Reads an invitation of the workspace, expired or not.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/invitations/{invitation_id}",
    tag = "invitations",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("invitation_id" = uuid::Uuid, Path, description = "The invitation."),
    ),
    responses(
        (status = 200, description = "The invitation.", body = Invitation,
            headers(("etag" = String, description = "The invitation's version, which the `If-Match` of a change of its role, or of a refresh, names (API-13)."))),
    )
)]
async fn workspace_invitation(
    authorize: Authorize<InvitationsRead>,
    params: PathIds,
) -> Result<ResourceResponse<Invitation>, Problem<InvitationsError>> {
    let id = params.id("invitation_id");
    let (mut uow, authorized) = authorize.open().await.map_err(InvitationsError::from)?;
    let id = id.ok_or(InvitationsError::NotFound)?;
    let current = service::invitation(&mut uow, &authorized, id).await?;
    Ok(ResourceResponse::ok(current.invitation).tagged(current.version))
}

/// What changing an invitation's role takes: a JSON Merge Patch of the invitation
/// (INVITATIONS-013).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeInvitationRoleRequest {
    /// The role the invitation is to offer, which must rank below the caller's
    /// (INVITATIONS-014).
    #[schema(value_type = Role)]
    role: String,
}

impl TryFrom<ChangeInvitationRoleRequest> for NewRole {
    type Error = RequestError;

    fn try_from(request: ChangeInvitationRoleRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let role = errors.check("/role", Role::parse(&request.role));
        errors.finish(role.map(|role| Self { role }))
    }
}

/// Changes the role of an invitation at a role below the caller's, to a role below the caller's,
/// sending no email.
#[utoipa::path(
    patch,
    path = "/v1/workspaces/{workspace_id}/invitations/{invitation_id}",
    tag = "invitations",
    request_body(content = ChangeInvitationRoleRequest, content_type = "application/merge-patch+json"),
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("invitation_id" = uuid::Uuid, Path, description = "The invitation whose role to change."),
        ("If-Match" = Option<String>, Header,
            description = "The `ETag` of the version the client changes, so that a change made since it read the invitation fails rather than being overwritten (API-13)."),
    ),
    responses(
        (status = 200, description = "The invitation, at the role.", body = Invitation,
            headers(("etag" = String, description = "The invitation's new version (API-13)."))),
    )
)]
async fn change_invitation_role(
    authorize: Authorize<InvitationsUpdate>,
    params: PathIds,
    if_match: IfMatch,
    request: MergePatch<ChangeInvitationRoleRequest>,
) -> Result<ResourceResponse<Invitation>, Problem<InvitationsError>> {
    let id = params.id("invitation_id");
    let role = NewRole::try_from(request.0).map_err(InvitationsError::from)?;
    let (uow, authorized) = authorize.open().await.map_err(InvitationsError::from)?;
    let id = id.ok_or(InvitationsError::NotFound)?;
    let current = service::change_role(uow, &authorized, id, role, if_match.versions()).await?;
    Ok(ResourceResponse::ok(current.invitation).tagged(current.version))
}

/// Sends an invitation at a role below the caller's again, expired or not: the caller becomes its
/// inviter, and it lasts the configured days from now.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{workspace_id}/invitations/{invitation_id}/resend",
    tag = "invitations",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("invitation_id" = uuid::Uuid, Path, description = "The invitation to send again."),
    ),
    responses(
        (status = 200, description = "The invitation, sent again.", body = Invitation,
            headers(("etag" = String, description = "The invitation's new version (API-13)."))),
    )
)]
async fn resend_invitation(
    State(invitations): State<Invitations>,
    limits: RateLimits,
    authorize: Authorize<InvitationsCreate>,
    params: PathIds,
) -> Result<ResourceResponse<Invitation>, Problem<InvitationsError>> {
    let id = params.id("invitation_id");
    let current = service::resend(&limits, authorize, id, &invitations).await?;
    Ok(ResourceResponse::ok(current.invitation).tagged(current.version))
}

/// Cancels an invitation at a role below the caller's, expired or not.
#[utoipa::path(
    delete,
    path = "/v1/workspaces/{workspace_id}/invitations/{invitation_id}",
    tag = "invitations",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("invitation_id" = uuid::Uuid, Path, description = "The invitation to cancel."),
    ),
    responses(
        (status = 204, description = "The invitation was cancelled."),
    )
)]
async fn cancel_invitation(
    authorize: Authorize<InvitationsCancel>,
    params: PathIds,
) -> Result<StatusCode, Problem<InvitationsError>> {
    let id = params.id("invitation_id");
    let (uow, authorized) = authorize.open().await.map_err(InvitationsError::from)?;
    let id = id.ok_or(InvitationsError::NotFound)?;
    service::cancel(uow, &authorized, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
