//! A superadmin's routes on the deployment's workspaces: listing every one, and deleting any
//! (the workspaces spec's "A superadmin's routes").

use axum::{extract::RawPathParams, http::StatusCode};
use platform::{
    authz::AuthorizeSuperadmin,
    db::Database,
    errors::{Problem, RequestError},
    http::{FieldErrors, Query, Routes, path_id},
    lists::{Cursor, Limit, List, Page, Search},
    types::WorkspaceId,
};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::routes;

use crate::modules::workspaces::{
    error::WorkspacesError, model::WorkspaceOverview, service::superadmin as service,
};

/// A superadmin's routes, which take the database as their state, as the module's others do.
pub(super) fn routes() -> Routes<Database> {
    Routes::new().superadmin(routes!(every), &[]).superadmin(
        routes!(delete_any),
        &[
            &RequestError::ReauthenticationRequired,
            &WorkspacesError::NotFound,
        ],
    )
}

/// The query of `GET /v1/superadmin/workspaces` (WORKSPACES-029).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct EveryQuery {
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

/// Lists every workspace of the deployment, the latest created first, each with its owner and
/// how many members it has, and nothing of its content.
#[utoipa::path(
    get,
    path = "/v1/superadmin/workspaces",
    tag = "workspaces",
    params(EveryQuery),
    responses(
        (status = 200, description = "A page of the deployment's workspaces.", body = List<WorkspaceOverview>),
    )
)]
async fn every(
    authorize: AuthorizeSuperadmin,
    query: Query<EveryQuery>,
) -> Result<axum::Json<List<WorkspaceOverview>>, Problem<WorkspacesError>> {
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
    let (mut uow, superadmin) = authorize.open().await.map_err(WorkspacesError::from)?;
    let list = service::every(&mut uow, &superadmin, search.as_ref(), &page).await?;
    Ok(axum::Json(list))
}

/// Deletes a workspace of the deployment, with everything in it, whoever its members are.
#[utoipa::path(
    delete,
    path = "/v1/superadmin/workspaces/{workspace_id}",
    tag = "workspaces",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
    ),
    responses(
        (status = 204, description = "The workspace is deleted, with everything in it."),
    )
)]
async fn delete_any(
    authorize: AuthorizeSuperadmin,
    params: RawPathParams,
) -> Result<StatusCode, Problem<WorkspacesError>> {
    let workspace = path_id(&params, "workspace_id").map(WorkspaceId::from_uuid);
    let (uow, superadmin) = authorize.open().await.map_err(WorkspacesError::from)?;
    service::delete(uow, &superadmin, workspace).await?;
    Ok(StatusCode::NO_CONTENT)
}
