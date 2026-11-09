//! Creating, listing, reading and renaming workspaces, and the members endpoints (the workspaces
//! spec's Routes).

use axum::{
    extract::{RawPathParams, State},
    http::StatusCode,
};
use platform::{
    authn::Principal,
    authz::Authorize,
    db::Database,
    errors::{Problem, RequestError},
    http::{
        FieldErrors, Idempotent, IfMatch, MergePatch, Query, ResourceResponse, Routes, path_id,
    },
    idempotency::IdempotencyKey,
    lists::{self, Cursor, Limit, List, Page, Search},
    types::{Client, UserId, WorkspaceId},
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::routes;

use super::{
    error::WorkspacesError,
    events::{MEMBER_LEFT, MEMBER_REMOVED, MEMBERSHIP_CREATED, WORKSPACE_RENAMED},
    model::{Member, NewWorkspace, Rename, Workspace, WorkspaceItem, WorkspaceName},
    service,
};
use crate::modules::access::{MembersRead, MembersRemove, Role, WorkspaceRead, WorkspaceUpdate};

/// The module's routes, which take the database as their state, and the events they publish.
pub(crate) fn routes() -> Routes<Database> {
    Routes::new()
        .events(&[
            &MEMBERSHIP_CREATED,
            &WORKSPACE_RENAMED,
            &MEMBER_REMOVED,
            &MEMBER_LEFT,
        ])
        .user(routes!(create, list), &[])
        .workspace::<WorkspaceRead>(routes!(read), &[])
        .workspace::<WorkspaceUpdate>(routes!(rename), &[])
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
        ("Idempotency-Key" = inline(Option<IdempotencyKey>), Header,
            description = "A key of the caller's own, as a quoted string, so a retry creates nothing twice (API-12)."),
    ),
    responses(
        (status = 201, description = "The new workspace, which the caller owns.", body = Workspace,
            headers(
                ("location" = String, description = "The new workspace: `/v1/workspaces/{id}`."),
                ("etag" = String, description = "The workspace's version, which a rename's `If-Match` names (API-13)."),
            )),
    )
)]
async fn create(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
    request: Idempotent<CreateRequest>,
) -> Result<ResourceResponse<Workspace>, Problem<WorkspacesError>> {
    let input = NewWorkspace::try_from(request.body).map_err(WorkspacesError::from)?;
    let created =
        service::create(&database, &client, &principal, request.key.as_ref(), input).await?;
    let location = format!("/v1/workspaces/{}", created.workspace.id);
    Ok(ResourceResponse::created(created.workspace, location).tagged(created.version))
}

/// The query of `GET /v1/workspaces` (WORKSPACES-003).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListQuery {
    /// How many workspaces a page holds.
    #[param(value_type = Option<Limit>, inline)]
    limit: Option<String>,
    /// The `next_cursor` of the page before.
    #[param(value_type = Option<Cursor>, inline)]
    cursor: Option<String>,
    /// Only the workspaces whose name contains this, ignoring case (API-20).
    #[param(value_type = Option<Search>, inline)]
    search: Option<String>,
}

/// Lists the workspaces the caller belongs to, the latest joined first.
#[utoipa::path(
    get,
    path = "/v1/workspaces",
    tag = "workspaces",
    params(ListQuery),
    responses(
        (status = 200, description = "A page of the caller's workspaces.", body = List<WorkspaceItem>),
    )
)]
async fn list(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
    query: Query<ListQuery>,
) -> Result<axum::Json<List<WorkspaceItem>>, Problem<WorkspacesError>> {
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
        .map_err(WorkspacesError::from)?;
    let list = service::list(&database, &client, &principal, search.as_ref(), &page).await?;
    Ok(axum::Json(list))
}

/// Reads a workspace the caller belongs to.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}",
    tag = "workspaces",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
    ),
    responses(
        (status = 200, description = "The workspace, with the caller's role.", body = Workspace,
            headers(("etag" = String, description = "The workspace's version, which a rename's `If-Match` names (API-13)."))),
    )
)]
async fn read(
    authorize: Authorize<WorkspaceRead>,
) -> Result<ResourceResponse<Workspace>, Problem<WorkspacesError>> {
    let (mut uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    let current = service::read(&mut uow, &authorized).await?;
    Ok(ResourceResponse::ok(current.workspace).tagged(current.version))
}

/// What renaming a workspace takes: a JSON Merge Patch of it (WORKSPACES-008).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameRequest {
    /// The workspace's new name (WORKSPACES-002).
    #[schema(value_type = WorkspaceName)]
    name: String,
}

impl TryFrom<RenameRequest> for Rename {
    type Error = RequestError;

    fn try_from(request: RenameRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let name = errors.check("/name", WorkspaceName::parse(&request.name));
        errors.finish(name.map(|name| Self { name }))
    }
}

/// Renames a workspace.
#[utoipa::path(
    patch,
    path = "/v1/workspaces/{workspace_id}",
    tag = "workspaces",
    request_body(content = RenameRequest, content_type = "application/merge-patch+json"),
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("If-Match" = Option<String>, Header,
            description = "The `ETag` of the version the client renames, so that a rename made since it read the workspace fails rather than being overwritten (API-13)."),
    ),
    responses(
        (status = 200, description = "The workspace, renamed.", body = Workspace,
            headers(("etag" = String, description = "The workspace's new version (API-13)."))),
    )
)]
async fn rename(
    authorize: Authorize<WorkspaceUpdate>,
    if_match: IfMatch,
    request: MergePatch<RenameRequest>,
) -> Result<ResourceResponse<Workspace>, Problem<WorkspacesError>> {
    let rename = Rename::try_from(request.0).map_err(WorkspacesError::from)?;
    let (uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    let current = service::rename(uow, &authorized, rename, if_match.versions()).await?;
    Ok(ResourceResponse::ok(current.workspace).tagged(current.version))
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
