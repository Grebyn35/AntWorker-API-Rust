//! A superadmin's routes on the deployment's workspaces: listing every one (the workspaces
//! spec's "A superadmin's routes").

use platform::{
    authz::AuthorizeSuperadmin,
    db::Database,
    errors::Problem,
    http::{FieldErrors, Query, Routes},
    lists::{Cursor, Limit, List, Page, Search},
};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::routes;

use crate::modules::workspaces::{
    error::WorkspacesError, model::WorkspaceOverview, service::superadmin as service,
};

/// A superadmin's routes, which take the database as their state, as the module's others do.
pub(super) fn routes() -> Routes<Database> {
    Routes::new().superadmin(routes!(every), &[])
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
