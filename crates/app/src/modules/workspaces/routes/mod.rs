//! Creating, listing, reading, renaming, transferring, deleting and leaving workspaces (the
//! workspaces spec's Routes). The members endpoints are in `members.rs`.

mod members;

use axum::{extract::State, http::StatusCode};
use platform::{
    authn::Principal,
    authz::Authorize,
    db::Database,
    errors::{Problem, RequestError},
    http::{FieldErrors, Idempotent, IfMatch, Json, MergePatch, Query, ResourceResponse, Routes},
    idempotency::IdempotencyKey,
    lists::{Cursor, Limit, List, Page, Search},
    types::{Client, WorkspaceId},
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::routes;

use super::{
    error::WorkspacesError,
    events::{
        MEMBER_LEFT, MEMBER_REMOVED, MEMBER_ROLE_CHANGED, MEMBERSHIP_CREATED, WORKSPACE_RENAMED,
        WORKSPACE_TRANSFERRED,
    },
    model::{MemberId, NewWorkspace, Rename, Transfer, Workspace, WorkspaceItem, WorkspaceName},
    service,
};
use crate::modules::access::{
    WorkspaceDelete, WorkspaceLeave, WorkspaceRead, WorkspaceTransfer, WorkspaceUpdate,
};

/// The module's routes, which take the database as their state, and the events they publish.
pub(crate) fn routes() -> Routes<Database> {
    Routes::new()
        .events(&[
            &MEMBERSHIP_CREATED,
            &WORKSPACE_RENAMED,
            &WORKSPACE_TRANSFERRED,
            &MEMBER_ROLE_CHANGED,
            &MEMBER_REMOVED,
            &MEMBER_LEFT,
        ])
        .user(routes!(create, list), &[])
        .workspace::<WorkspaceRead>(routes!(read), &[])
        .workspace::<WorkspaceUpdate>(routes!(rename), &[])
        .workspace::<WorkspaceDelete>(routes!(delete), &[&RequestError::ReauthenticationRequired])
        .workspace::<WorkspaceTransfer>(
            routes!(transfer),
            &[
                &RequestError::ReauthenticationRequired,
                &WorkspacesError::TransferRequiresAdmin,
                &WorkspacesError::MemberNotFound,
            ],
        )
        .workspace::<WorkspaceLeave>(routes!(leave), &[])
        .merge(members::routes())
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

/// Deletes a workspace, with everything in it.
#[utoipa::path(
    delete,
    path = "/v1/workspaces/{workspace_id}",
    tag = "workspaces",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
    ),
    responses(
        (status = 204, description = "The workspace is deleted, with everything in it."),
    )
)]
async fn delete(
    authorize: Authorize<WorkspaceDelete>,
) -> Result<StatusCode, Problem<WorkspacesError>> {
    let (uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    service::delete(uow, &authorized).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What transferring a workspace takes (WORKSPACES-015).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransferRequest {
    /// The member who is to own the workspace, an admin (WORKSPACES-016).
    #[schema(value_type = MemberId)]
    user_id: String,
}

impl TryFrom<TransferRequest> for Transfer {
    type Error = RequestError;

    fn try_from(request: TransferRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let to = errors.check("/user_id", MemberId::parse(&request.user_id));
        errors.finish(to.map(|to| Self { to: to.user_id() }))
    }
}

/// Transfers a workspace to one of its admins, and makes the caller an admin.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{workspace_id}/transfer",
    tag = "workspaces",
    request_body = TransferRequest,
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
    ),
    responses(
        (status = 204, description = "The member owns the workspace, and the caller is an admin."),
    )
)]
async fn transfer(
    authorize: Authorize<WorkspaceTransfer>,
    request: Json<TransferRequest>,
) -> Result<StatusCode, Problem<WorkspacesError>> {
    let transfer = Transfer::try_from(request.0).map_err(WorkspacesError::from)?;
    let (uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    service::transfer(uow, &authorized, transfer).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Leaves a workspace, as every member may but its owner, who transfers it first, or deletes it.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{workspace_id}/leave",
    tag = "workspaces",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
    ),
    responses(
        (status = 204, description = "The caller is no member of the workspace."),
    )
)]
async fn leave(
    authorize: Authorize<WorkspaceLeave>,
) -> Result<StatusCode, Problem<WorkspacesError>> {
    let (uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    service::leave(uow, &authorized).await?;
    Ok(StatusCode::NO_CONTENT)
}
