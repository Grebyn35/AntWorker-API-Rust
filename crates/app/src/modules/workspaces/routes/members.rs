//! The members endpoints: listing a workspace's members, reading one, changing a member's role,
//! and removing a member (the workspaces spec's Routes).

use axum::{extract::RawPathParams, http::StatusCode};
use platform::{
    authz::Authorize,
    db::Database,
    errors::{Problem, RequestError},
    http::{FieldErrors, IfMatch, MergePatch, Query, ResourceResponse, Routes, path_id},
    lists::{self, Cursor, Limit, List, Page, Search},
    types::{UserId, WorkspaceId},
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::routes;

use crate::modules::{
    access::{MembersRead, MembersRemove, MembersUpdate, Role},
    workspaces::{
        error::WorkspacesError,
        model::{Member, NewRole},
        service,
    },
};

/// The routes of members, which take the database as their state.
pub(super) fn routes() -> Routes<Database> {
    Routes::new()
        .workspace::<MembersRead>(routes!(members), &[])
        .workspace::<MembersRead>(routes!(member), &[&WorkspacesError::MemberNotFound])
        .workspace::<MembersUpdate>(routes!(change_role), &[&WorkspacesError::MemberNotFound])
        .workspace::<MembersRemove>(
            routes!(remove_member),
            &[
                &WorkspacesError::CannotRemoveSelf,
                &WorkspacesError::MemberNotFound,
            ],
        )
}

/// The query of `GET /v1/workspaces/{workspace_id}/members` (WORKSPACES-004).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct MembersQuery {
    /// How many members a page holds.
    #[param(value_type = Option<Limit>, inline)]
    limit: Option<String>,
    /// The `next_cursor` of the page before.
    #[param(value_type = Option<Cursor>, inline)]
    cursor: Option<String>,
    /// Only the members whose name or email contains this, ignoring case (API-20).
    #[param(value_type = Option<Search>, inline)]
    search: Option<String>,
    /// Only the members of this role.
    #[param(value_type = Option<Role>)]
    role: Option<String>,
}

/// Lists the workspace's members, the latest joined first.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/members",
    tag = "workspaces",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        MembersQuery,
    ),
    responses(
        (status = 200, description = "A page of the workspace's members.", body = List<Member>),
    )
)]
async fn members(
    authorize: Authorize<MembersRead>,
    query: Query<MembersQuery>,
) -> Result<axum::Json<List<Member>>, Problem<WorkspacesError>> {
    let params = query.params();
    let mut errors = FieldErrors::new();
    let page = Page::read(
        query.route(),
        params.limit.as_deref(),
        params.cursor.as_deref(),
        &mut errors,
    );
    let search = Search::read(params.search.as_deref(), &mut errors);
    let role = lists::parameter(&mut errors, "role", params.role.as_deref(), Role::parse);
    let (page, (search, role)) = errors
        .finish_query(page.zip(search.zip(role)))
        .map_err(WorkspacesError::from)?;
    let (mut uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    let list = service::members(&mut uow, &authorized, role, search.as_ref(), &page).await?;
    Ok(axum::Json(list))
}

/// Reads a member of the workspace.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/members/{user_id}",
    tag = "workspaces",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("user_id" = UserId, Path, description = "The member."),
    ),
    responses(
        (status = 200, description = "The member.", body = Member,
            headers(("etag" = String, description = "The version of the member's membership, which a change of their role's `If-Match` names (API-13)."))),
    )
)]
async fn member(
    authorize: Authorize<MembersRead>,
    params: RawPathParams,
) -> Result<ResourceResponse<Member>, Problem<WorkspacesError>> {
    let user = path_id(&params, "user_id").map(UserId::from_uuid);
    let (mut uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    let user = user.ok_or(WorkspacesError::MemberNotFound)?;
    let current = service::member(&mut uow, &authorized, user).await?;
    Ok(ResourceResponse::ok(current.member).tagged(current.version))
}

/// What changing a member's role takes: a JSON Merge Patch of the member (WORKSPACES-012).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeRoleRequest {
    /// The role the member is to hold, which must rank below the caller's (WORKSPACES-013).
    #[schema(value_type = Role)]
    role: String,
}

impl TryFrom<ChangeRoleRequest> for NewRole {
    type Error = RequestError;

    fn try_from(request: ChangeRoleRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let role = errors.check("/role", Role::parse(&request.role));
        errors.finish(role.map(|role| Self { role }))
    }
}

/// Changes the role of a member of lower rank than the caller, to a role below the caller's.
#[utoipa::path(
    patch,
    path = "/v1/workspaces/{workspace_id}/members/{user_id}",
    tag = "workspaces",
    request_body(content = ChangeRoleRequest, content_type = "application/merge-patch+json"),
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("user_id" = UserId, Path, description = "The member whose role to change."),
        ("If-Match" = Option<String>, Header,
            description = "The `ETag` of the version the client changes, so that a change made since it read the member fails rather than being overwritten (API-13)."),
    ),
    responses(
        (status = 200, description = "The member, at the role.", body = Member,
            headers(("etag" = String, description = "The new version of the member's membership (API-13)."))),
    )
)]
async fn change_role(
    authorize: Authorize<MembersUpdate>,
    params: RawPathParams,
    if_match: IfMatch,
    request: MergePatch<ChangeRoleRequest>,
) -> Result<ResourceResponse<Member>, Problem<WorkspacesError>> {
    let user = path_id(&params, "user_id").map(UserId::from_uuid);
    let role = NewRole::try_from(request.0).map_err(WorkspacesError::from)?;
    let (uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    let user = user.ok_or(WorkspacesError::MemberNotFound)?;
    let current = service::change_role(uow, &authorized, user, role, if_match.versions()).await?;
    Ok(ResourceResponse::ok(current.member).tagged(current.version))
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
    let user = path_id(&params, "user_id").map(UserId::from_uuid);
    let (uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    let user = user.ok_or(WorkspacesError::MemberNotFound)?;
    service::remove_member(uow, &authorized, user).await?;
    Ok(StatusCode::NO_CONTENT)
}
