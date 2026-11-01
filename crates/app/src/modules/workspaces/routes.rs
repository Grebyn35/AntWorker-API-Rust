//! Creating and listing workspaces, and the members endpoints (the workspaces spec's Routes).

use axum::{
    extract::{RawPathParams, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use platform::{
    authn::Principal,
    authz::Authorize,
    db::Database,
    errors::{Internal, Problem, RequestError},
    http::{FieldErrors, Idempotent, Query, Routes},
    lists::{Limit, List, Page, Search},
    types::{Client, UserId, WorkspaceId},
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::{
    error::WorkspacesError,
    model::{Created, Member, NewWorkspace, WorkspaceItem, WorkspaceName},
    service,
};
use crate::modules::access::{MembersRead, MembersRemove, Role};

/// The module's routes, which take the database as their state.
pub(crate) fn routes() -> Routes<Database> {
    Routes::new()
        .user(routes!(create, list), &[])
        .workspace::<MembersRead>(routes!(members), &[])
        .workspace::<MembersRemove>(
            routes!(remove_member),
            &[
                &WorkspacesError::CannotRemoveSelf,
                &WorkspacesError::MemberNotFound,
            ],
        )
}

/// What creating a workspace takes (WORKSPACES-001).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    /// The workspace's name (WORKSPACES-002).
    #[schema(value_type = WorkspaceName)]
    name: String,
}

impl TryFrom<CreateRequest> for NewWorkspace {
    type Error = RequestError;

    fn try_from(request: CreateRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let name = errors.check("/name", WorkspaceName::parse(&request.name));
        errors.finish(name.map(|name| Self { name }))
    }
}

/// Creates a workspace that the caller owns.
#[utoipa::path(
    post,
    path = "/v1/workspaces",
    tag = "workspaces",
    request_body = CreateRequest,
    params(
        ("Idempotency-Key" = Option<String>, Header,
            description = "A key of the caller's own, as a quoted string, so a retry creates nothing twice (API-12)."),
    ),
    responses(
        (status = 201, description = "The new workspace, which the caller owns.", body = Created,
            headers(("location" = String, description = "The new workspace: `/v1/workspaces/{id}`."))),
    )
)]
async fn create(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
    request: Idempotent<CreateRequest>,
) -> Result<Response, Problem<WorkspacesError>> {
    let input = NewWorkspace::try_from(request.body).map_err(WorkspacesError::from)?;
    let created =
        service::create(&database, &client, &principal, request.key.as_ref(), input).await?;
    let location = HeaderValue::try_from(format!("/v1/workspaces/{}", created.id))
        .map_err(|error| WorkspacesError::Internal(Internal::new(error)))?;
    let mut response = (StatusCode::CREATED, axum::Json(created)).into_response();
    response.headers_mut().insert(header::LOCATION, location);
    Ok(response)
}

/// Lists the workspaces the caller belongs to, the latest joined first.
#[utoipa::path(
    get,
    path = "/v1/workspaces",
    tag = "workspaces",
    params(
        ("limit" = inline(Option<Limit>), Query, description = "How many workspaces a page holds."),
        ("cursor" = Option<String>, Query, description = "The `next_cursor` of the page before."),
        ("search" = inline(Option<Search>), Query, description = "Only the workspaces whose name contains this, ignoring case (API-20)."),
    ),
    responses(
        (status = 200, description = "A page of the caller's workspaces.", body = List<WorkspaceItem>),
    )
)]
async fn list(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
    query: Query,
) -> Result<axum::Json<List<WorkspaceItem>>, Problem<WorkspacesError>> {
    let mut errors = FieldErrors::new();
    let page = Page::read(&query, &mut errors);
    let search = Search::read(&query, &mut errors);
    let (page, search) = errors
        .finish_query(page.zip(search))
        .map_err(WorkspacesError::from)?;
    let list = service::list(&database, &client, &principal, search.as_ref(), &page).await?;
    Ok(axum::Json(list))
}

/// Lists the workspace's members, the latest joined first.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/members",
    tag = "workspaces",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("limit" = inline(Option<Limit>), Query, description = "How many members a page holds."),
        ("cursor" = Option<String>, Query, description = "The `next_cursor` of the page before."),
        ("search" = inline(Option<Search>), Query, description = "Only the members whose name or email contains this, ignoring case (API-20)."),
        ("role" = Option<Role>, Query, description = "Only the members of this role."),
    ),
    responses(
        (status = 200, description = "A page of the workspace's members.", body = List<Member>),
    )
)]
async fn members(
    authorize: Authorize<MembersRead>,
    query: Query,
) -> Result<axum::Json<List<Member>>, Problem<WorkspacesError>> {
    let mut errors = FieldErrors::new();
    let page = Page::read(&query, &mut errors);
    let search = Search::read(&query, &mut errors);
    let role = query.read(&mut errors, "role", Role::parse);
    let (page, (search, role)) = errors
        .finish_query(page.zip(search.zip(role)))
        .map_err(WorkspacesError::from)?;
    let (mut uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    let list = service::members(&mut uow, &authorized, role, search.as_ref(), &page).await?;
    Ok(axum::Json(list))
}

/// Removes a member of lower rank than the caller.
#[utoipa::path(
    delete,
    path = "/v1/workspaces/{workspace_id}/members/{user_id}",
    tag = "workspaces",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("user_id" = UserId, Path, description = "The member to remove."),
    ),
    responses(
        (status = 204, description = "The member was removed."),
    )
)]
async fn remove_member(
    authorize: Authorize<MembersRemove>,
    params: RawPathParams,
) -> Result<StatusCode, Problem<WorkspacesError>> {
    // A user ID that isn't a UUID names no member.
    let user = params
        .iter()
        .find(|(name, _)| *name == "user_id")
        .and_then(|(_, value)| uuid::Uuid::parse_str(value).ok())
        .map(UserId::from_uuid);
    let (uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    let user = user.ok_or(WorkspacesError::MemberNotFound)?;
    service::remove_member(uow, &authorized, user).await?;
    Ok(StatusCode::NO_CONTENT)
}
