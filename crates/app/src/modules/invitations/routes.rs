//! Inviting, listing the caller's invitations, and accepting one (the invitations spec's Routes).

use axum::{
    extract::{RawPathParams, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use platform::{
    authn::Principal,
    authz::Authorize,
    errors::{Internal, Problem, RequestError},
    http::{FieldErrors, Idempotent, Query, Routes},
    lists::{Limit, List, Page, Search},
    types::{Client, EmailAddress, WorkspaceId},
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::routes;

use super::{
    error::InvitationsError,
    model::{Accepted, Invitation, NewInvitation, ReceivedInvitation},
    service::{self, Invitations},
};
use crate::modules::access::{InvitationsCreate, Role};

/// The module's routes, which take its state.
pub(crate) fn routes() -> Routes<Invitations> {
    Routes::new()
        .workspace::<InvitationsCreate>(routes!(invite), &[&InvitationsError::AlreadyMember])
        .user(routes!(received), &[])
        .user(
            routes!(accept),
            &[
                &InvitationsError::NotFound,
                &InvitationsError::Expired,
                &InvitationsError::AlreadyMember,
            ],
        )
}

/// What inviting takes (INVITATIONS-001).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InviteRequest {
    /// The invitee's address (ACCOUNTS-002).
    #[schema(value_type = EmailAddress)]
    email: String,
    /// The role to offer, which must rank below the inviter's (ACCESS-004).
    #[schema(value_type = Role)]
    role: String,
}

impl TryFrom<InviteRequest> for NewInvitation {
    type Error = RequestError;

    fn try_from(request: InviteRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let email = errors.check("/email", EmailAddress::parse(&request.email));
        let role = errors.check("/role", Role::parse(&request.role));
        errors.finish(email.zip(role).map(|(email, role)| Self { email, role }))
    }
}

/// Invites an email address to the workspace, at a role below the caller's.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{workspace_id}/invitations",
    tag = "invitations",
    request_body = InviteRequest,
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("Idempotency-Key" = Option<String>, Header,
            description = "A key of the caller's own, as a quoted string, so a retry invites once (API-12)."),
    ),
    responses(
        (status = 201, description = "The new invitation.", body = Invitation,
            headers(("location" = String, description = "The new invitation: `/v1/workspaces/{workspace_id}/invitations/{id}`."))),
        (status = 200, description = "The address's invitation to the workspace, refreshed in place.", body = Invitation),
    )
)]
async fn invite(
    State(invitations): State<Invitations>,
    authorize: Authorize<InvitationsCreate>,
    request: Idempotent<InviteRequest>,
) -> Result<Response, Problem<InvitationsError>> {
    let input = NewInvitation::try_from(request.body).map_err(InvitationsError::from)?;
    let (uow, authorized) = authorize.open().await.map_err(InvitationsError::from)?;
    let invited =
        service::invite(uow, &authorized, request.key.as_ref(), input, &invitations).await?;
    let invitation = invited.invitation;
    if !invited.created {
        return Ok(axum::Json(invitation).into_response());
    }
    let location = HeaderValue::try_from(format!(
        "/v1/workspaces/{}/invitations/{}",
        invitation.workspace_id, invitation.id
    ))
    .map_err(|error| InvitationsError::Internal(Internal::new(error)))?;
    let mut response = (StatusCode::CREATED, axum::Json(invitation)).into_response();
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

/// The query of `GET /v1/users/me/invitations` (INVITATIONS-004).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceivedQuery {
    /// How many invitations a page holds.
    #[param(value_type = Option<Limit>, inline)]
    limit: Option<String>,
    /// The `next_cursor` of the page before.
    cursor: Option<String>,
    /// Only the invitations to workspaces whose name contains this, ignoring case (API-20).
    #[param(value_type = Option<Search>, inline)]
    search: Option<String>,
}

/// Lists the caller's unexpired invitations in every workspace, the latest first.
#[utoipa::path(
    get,
    path = "/v1/users/me/invitations",
    tag = "invitations",
    params(ReceivedQuery),
    responses(
        (status = 200, description = "A page of the caller's invitations.", body = List<ReceivedInvitation>),
    )
)]
async fn received(
    State(invitations): State<Invitations>,
    client: Client,
    principal: Principal,
    query: Query<ReceivedQuery>,
) -> Result<axum::Json<List<ReceivedInvitation>>, Problem<InvitationsError>> {
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
    let list = service::received(
        &invitations.database,
        &client,
        &principal,
        search.as_ref(),
        &page,
    )
    .await?;
    Ok(axum::Json(list))
}

/// Accepts an invitation addressed to the caller, who joins its workspace at its role.
#[utoipa::path(
    post,
    path = "/v1/users/me/invitations/{invitation_id}/accept",
    tag = "invitations",
    params(
        ("invitation_id" = uuid::Uuid, Path, description = "The invitation."),
    ),
    responses(
        (status = 200, description = "The workspace the caller joined, at their role.", body = Accepted),
    )
)]
async fn accept(
    State(invitations): State<Invitations>,
    client: Client,
    principal: Principal,
    params: RawPathParams,
) -> Result<axum::Json<Accepted>, Problem<InvitationsError>> {
    // An ID that isn't a UUID names no invitation.
    let id = params
        .iter()
        .find(|(name, _)| *name == "invitation_id")
        .and_then(|(_, value)| uuid::Uuid::parse_str(value).ok());
    let accepted = service::accept(&invitations.database, &client, &principal, id).await?;
    Ok(axum::Json(accepted))
}
